"""Read-only S3-compatible object access for the derived index's source plane (CORE-007, INT-011).

Artifact bytes are written by the Rust artifact authority and are immutable per version
(DOMAIN.md §10); the intelligence plane only ever *reads* them, to index their text. This
module is that read, and it is deliberately the narrowest thing that can be: list the keys
under a prefix and fetch one object. There is no write, no delete and no multipart surface,
because writing artifact bytes is not the intelligence plane's authority.

SigV4 is implemented here with the standard library only. The intelligence plane ships no
provider SDK, and the caching/minio SDKs would add a dependency for two verbs. The signing
rules are CORE-007's; the boundary variables are the same ones its client documents:

* `QUANSIO_TEST_MINIO_ENDPOINT` — default `http://127.0.0.1:59010`
* `QUANSIO_TEST_MINIO_REGION` — default `us-east-1`
* `QUANSIO_TEST_MINIO_BUCKET` — default `quansio-dev`
* `QUANSIO_TEST_MINIO_ACCESS_KEY` — default `quansio-dev`
* `QUANSIO_TEST_MINIO_SECRET_KEY` — default `quansio-dev-only` (dev only)
* `QUANSIO_TEST_MINIO_SESSION_TOKEN` — optional
"""

from __future__ import annotations

import datetime
import hashlib
import hmac
import http.client
import os
import urllib.parse
import xml.etree.ElementTree as ElementTree
from collections.abc import Mapping
from dataclasses import dataclass

#: Dev-stack defaults, matching `config/dev.yaml` and CORE-007's documented boundary.
DEV_ENDPOINT = "http://127.0.0.1:59010"
DEV_REGION = "us-east-1"
DEV_BUCKET = "quansio-dev"
DEV_ACCESS_KEY = "quansio-dev"
DEV_SECRET_KEY = "quansio-dev-only"

#: Rules, named so a caller can tell which refusal fired.
RULE_OBJECT_MISSING = "object.missing"
RULE_OBJECT_UNAVAILABLE = "object.unavailable"
RULE_OBJECT_CONFIG = "object.config"
RULE_OBJECT_RESPONSE = "object.response"

#: Largest object this reader will fetch: a source document, not an arbitrary binary.
MAX_OBJECT_BYTES = 8 * 1024 * 1024
#: Largest listing page the reader asks for.
LIST_PAGE_SIZE = 1_000
#: Pages one listing may walk before it refuses, so a huge bucket cannot loop forever.
MAX_LIST_PAGES = 100


class ObjectReadError(RuntimeError):
    """An object could not be read; never a partial or substituted body."""

    def __init__(self, code: str, rule_id: str, detail: str) -> None:
        super().__init__(detail)
        self.code = code
        self.rule_id = rule_id
        self.detail = detail


@dataclass(frozen=True, slots=True)
class S3Config:
    """Where artifact bytes live and what unlocks them (never a production credential)."""

    endpoint: str
    region: str
    bucket: str
    access_key: str
    secret_key: str
    session_token: str = ""

    def __post_init__(self) -> None:
        if not self.bucket or "/" in self.bucket:
            raise ObjectReadError(
                "VALIDATION_SCHEMA", RULE_OBJECT_CONFIG, f"bucket {self.bucket!r} is not a bucket name"
            )
        parts = urllib.parse.urlsplit(self.endpoint)
        if parts.scheme not in {"http", "https"} or not parts.hostname:
            raise ObjectReadError(
                "VALIDATION_SCHEMA",
                RULE_OBJECT_CONFIG,
                f"endpoint {self.endpoint!r} is not an http(s) endpoint",
            )
        if not self.access_key or not self.secret_key:
            raise ObjectReadError(
                "VALIDATION_SCHEMA", RULE_OBJECT_CONFIG, "object storage access is not configured"
            )

    @property
    def host(self) -> str:
        parts = urllib.parse.urlsplit(self.endpoint)
        return parts.netloc

    @property
    def hostname(self) -> str:
        """Host to connect to; `__post_init__` already refused an endpoint without one."""
        return urllib.parse.urlsplit(self.endpoint).hostname or ""

    @property
    def port(self) -> int | None:
        return urllib.parse.urlsplit(self.endpoint).port

    @property
    def secure(self) -> bool:
        return urllib.parse.urlsplit(self.endpoint).scheme == "https"

    @classmethod
    def from_env(cls, environ: Mapping[str, str] | None = None) -> S3Config:
        """Build from the `QUANSIO_TEST_MINIO_*` boundary variables, dev defaults included."""
        env = os.environ if environ is None else environ
        return cls(
            endpoint=env.get("QUANSIO_TEST_MINIO_ENDPOINT", DEV_ENDPOINT).rstrip("/"),
            region=env.get("QUANSIO_TEST_MINIO_REGION", DEV_REGION),
            bucket=env.get("QUANSIO_TEST_MINIO_BUCKET", DEV_BUCKET),
            access_key=env.get("QUANSIO_TEST_MINIO_ACCESS_KEY", DEV_ACCESS_KEY),
            secret_key=env.get("QUANSIO_TEST_MINIO_SECRET_KEY", DEV_SECRET_KEY),
            session_token=env.get("QUANSIO_TEST_MINIO_SESSION_TOKEN", ""),
        )


def _sha256_hex(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def _signing_key(secret: str, datestamp: str, region: str) -> bytes:
    key = hmac.new(("AWS4" + secret).encode("utf-8"), datestamp.encode("utf-8"), hashlib.sha256).digest()
    for part in (region, "s3", "aws4_request"):
        key = hmac.new(key, part.encode("utf-8"), hashlib.sha256).digest()
    return key


def _quote(value: str) -> str:
    """RFC 3986 encoding, which SigV4 requires in both the URI and the canonical query."""
    return urllib.parse.quote(value, safe="-_.~")


@dataclass(slots=True)
class S3ObjectReader:
    """`ObjectBytes` over an S3-compatible endpoint: list a prefix, read one object."""

    config: S3Config

    @classmethod
    def from_env(cls, environ: Mapping[str, str] | None = None) -> S3ObjectReader:
        return cls(config=S3Config.from_env(environ))

    def read(self, object_key: str, *, limit: int = MAX_OBJECT_BYTES) -> bytes:
        """Fetch at most ``limit + 1`` bytes of one object.

        The extra byte is deliberate: a caller that gets back more than its limit knows the
        object is larger than it can handle and can refuse it without another request. Digest
        verification belongs to the reader that knows the recorded digest (the source reader);
        this method's contract is "these are the object's leading bytes, or a typed failure".
        """
        if not object_key.strip():
            raise ObjectReadError("VALIDATION_SCHEMA", RULE_OBJECT_CONFIG, "an object key is required")
        status, body = self._get(f"{self.config.bucket}/{object_key}", {}, limit=max(limit, 0) + 1)
        if status == 404:
            raise ObjectReadError(
                "NOT_FOUND", RULE_OBJECT_MISSING, f"object {object_key!r} is not in the bucket"
            )
        if status != 200:
            raise ObjectReadError(
                "PROVIDER_UNAVAILABLE",
                RULE_OBJECT_UNAVAILABLE,
                f"object storage answered HTTP {status} for {object_key!r}",
            )
        return body

    def list_keys(self, prefix: str) -> tuple[str, ...]:
        """Every key under a prefix, in the store's lexicographic order, walking pages."""
        keys: list[str] = []
        token = ""
        for _page in range(MAX_LIST_PAGES):
            params = {"list-type": "2", "max-keys": str(LIST_PAGE_SIZE), "prefix": prefix}
            if token:
                params["continuation-token"] = token
            status, body = self._get(self.config.bucket, params)
            if status != 200:
                raise ObjectReadError(
                    "PROVIDER_UNAVAILABLE",
                    RULE_OBJECT_UNAVAILABLE,
                    f"object storage answered HTTP {status} listing {prefix!r}",
                )
            try:
                root = ElementTree.fromstring(body)
            except ElementTree.ParseError as error:
                raise ObjectReadError(
                    "PROVIDER_UNAVAILABLE",
                    RULE_OBJECT_RESPONSE,
                    f"object storage returned a listing that is not XML ({prefix!r})",
                ) from error
            namespace = ""
            if root.tag.startswith("{"):
                namespace = root.tag.split("}", 1)[0] + "}"
            keys.extend(element.text or "" for element in root.findall(f"{namespace}Contents/{namespace}Key"))
            truncated = (root.findtext(f"{namespace}IsTruncated") or "").strip().lower() == "true"
            token = (root.findtext(f"{namespace}NextContinuationToken") or "").strip()
            if not truncated or not token:
                return tuple(keys)
        raise ObjectReadError(
            "VALIDATION_BOUNDS",
            RULE_OBJECT_RESPONSE,
            f"listing {prefix!r} exceeded {MAX_LIST_PAGES} pages",
        )

    def _get(
        self, resource: str, params: Mapping[str, str], *, limit: int = MAX_OBJECT_BYTES
    ) -> tuple[int, bytes]:
        """One signed GET of `resource` (bucket-relative path) with optional query parameters."""
        canonical_uri = "/" + urllib.parse.quote(resource, safe="/-_.~")
        canonical_query = "&".join(
            f"{_quote(name)}={_quote(value)}" for name, value in sorted(params.items())
        )
        url = canonical_uri + (f"?{canonical_query}" if canonical_query else "")
        headers = self._signed_headers(canonical_uri, canonical_query)
        connection_class = http.client.HTTPSConnection if self.config.secure else http.client.HTTPConnection
        connection = connection_class(
            self.config.hostname,
            self.config.port,
            timeout=30,
        )
        try:
            connection.request("GET", url, body=None, headers=headers)
            response = connection.getresponse()
            return response.status, response.read(limit)
        except (TimeoutError, OSError) as error:
            raise ObjectReadError(
                "PROVIDER_UNAVAILABLE",
                RULE_OBJECT_UNAVAILABLE,
                "object storage is unreachable",
            ) from error
        finally:
            connection.close()

    def _signed_headers(self, canonical_uri: str, canonical_query: str) -> dict[str, str]:
        """The `Authorization` header for one GET, signed over exactly what is sent."""
        now = datetime.datetime.now(datetime.UTC)
        amz_date = now.strftime("%Y%m%dT%H%M%SZ")
        datestamp = now.strftime("%Y%m%d")
        payload_hash = _sha256_hex(b"")
        headers = {
            "host": self.config.host,
            "x-amz-content-sha256": payload_hash,
            "x-amz-date": amz_date,
        }
        if self.config.session_token:
            headers["x-amz-security-token"] = self.config.session_token
        signed_names = sorted(headers)
        canonical_headers = "".join(f"{name}:{headers[name]}\n" for name in signed_names)
        signed_headers = ";".join(signed_names)
        canonical_request = "\n".join(
            [
                "GET",
                canonical_uri,
                canonical_query,
                canonical_headers,
                signed_headers,
                payload_hash,
            ]
        )
        scope = f"{datestamp}/{self.config.region}/s3/aws4_request"
        string_to_sign = "\n".join(
            ["AWS4-HMAC-SHA256", amz_date, scope, _sha256_hex(canonical_request.encode("utf-8"))]
        )
        signature = hmac.new(
            _signing_key(self.config.secret_key, datestamp, self.config.region),
            string_to_sign.encode("utf-8"),
            hashlib.sha256,
        ).hexdigest()
        authorization = (
            f"AWS4-HMAC-SHA256 Credential={self.config.access_key}/{scope}, "
            f"SignedHeaders={signed_headers}, Signature={signature}"
        )
        return {
            "Host": self.config.host,
            "Authorization": authorization,
            "x-amz-content-sha256": payload_hash,
            "x-amz-date": amz_date,
            **({"x-amz-security-token": self.config.session_token} if self.config.session_token else {}),
        }

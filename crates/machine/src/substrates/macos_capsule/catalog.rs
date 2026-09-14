//! Desktop-update guest image catalog (EXEC-003).
//!
//! The signed V8.1 Linux guest is the only image the capsule may boot. Its identity lives in
//! `config/guest-images.yaml`: channel, required artifacts, private virtio-socket port and the
//! isolation that makes host secrets and cloud metadata unreachable. An unpublished catalog has no
//! digest and no URL, so there is no artifact a controller could accept as real.

use super::{CapsuleError, QWORKERD_VSOCK_PORT};

/// The committed desktop-update catalog. Production boots are refused while it is unpublished.
pub const GUEST_IMAGES_YAML: &str = include_str!("../../../../../config/guest-images.yaml");

/// Isolation the native VM is allowed to present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleIsolation {
    /// Guest network devices. Zero means no IP path to host secrets or cloud metadata.
    pub network_devices: u32,
    /// Host directory shares. Zero means the guest cannot mount Keychain or host files.
    pub host_directory_shares: u32,
}

/// The desktop-update catalog entry for the macOS local capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestImageCatalog {
    /// Catalog schema version.
    pub schema_version: String,
    /// Update channel that ships or downloads the image.
    pub channel: String,
    /// Release status. Only `published` may boot.
    pub status: String,
    /// Artifacts the signed image must include.
    pub required_artifacts: Vec<String>,
    /// Guest contents the signed image must include.
    pub contents: Vec<String>,
    /// Private virtio-socket port qworkerd listens on.
    pub vsock_port: u32,
    /// Device-graph isolation.
    pub isolation: CapsuleIsolation,
    /// Why a non-published catalog cannot boot.
    pub blocker: Option<String>,
}

impl GuestImageCatalog {
    /// Load the embedded catalog. Missing required fields fail closed.
    ///
    /// # Errors
    /// Returns [`CapsuleError::CatalogInvalid`] when the YAML is not the shape this controller
    /// requires.
    pub fn load() -> Result<Self, CapsuleError> {
        let parsed: serde_yaml::Value = serde_yaml::from_str(GUEST_IMAGES_YAML)
            .map_err(|error| CapsuleError::CatalogInvalid(error.to_string()))?;
        let capsule = parsed
            .get("macos_local_capsule")
            .ok_or_else(|| CapsuleError::CatalogInvalid("missing macos_local_capsule".into()))?;
        let schema_version = string_field(&parsed, "schema_version")?;
        let channel = string_field(&parsed, "channel")?;
        let status = string_field(capsule, "status")?;
        let required_artifacts = string_list(capsule, "required_artifacts")?;
        let contents = string_list(capsule, "contents")?;
        let vsock_port = capsule
            .get("control_channel")
            .and_then(|value| value.get("port"))
            .and_then(serde_yaml::Value::as_u64)
            .ok_or_else(|| CapsuleError::CatalogInvalid("missing control_channel.port".into()))?;
        let vsock_port = u32::try_from(vsock_port).map_err(|_| {
            CapsuleError::CatalogInvalid("control_channel.port out of range".into())
        })?;
        let isolation = capsule
            .get("isolation")
            .ok_or_else(|| CapsuleError::CatalogInvalid("missing isolation".into()))?;
        let network_devices = u32_field(isolation, "network_devices")?;
        let host_directory_shares = u32_field(isolation, "host_directory_shares")?;
        let blocker = capsule
            .get("blocker")
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string);
        if vsock_port != QWORKERD_VSOCK_PORT {
            return Err(CapsuleError::CatalogInvalid(format!(
                "control_channel.port {vsock_port} is not the compiled qworkerd port {QWORKERD_VSOCK_PORT}"
            )));
        }
        if status != "published"
            && (capsule.get("sha256").is_some() || capsule.get("url").is_some())
        {
            return Err(CapsuleError::CatalogInvalid(
                "an unpublished catalog must not carry a sha256 or url".into(),
            ));
        }
        Ok(Self {
            schema_version,
            channel,
            status,
            required_artifacts,
            contents,
            vsock_port,
            isolation: CapsuleIsolation {
                network_devices,
                host_directory_shares,
            },
            blocker,
        })
    }

    /// Refuse to boot unless the catalog names a published, digest-pinned release.
    ///
    /// # Errors
    /// Returns [`CapsuleError::ImageUnavailable`] when the catalog is not `published`.
    pub fn require_published(&self) -> Result<(), CapsuleError> {
        if self.status == "published" {
            Ok(())
        } else {
            Err(CapsuleError::ImageUnavailable {
                status: self.status.clone(),
                reason: self
                    .blocker
                    .clone()
                    .unwrap_or_else(|| "no signed V8.1 guest image is published".to_string()),
            })
        }
    }

    /// Host secrets and cloud metadata are unreachable when the guest has no IP device and no
    /// host directory share.
    #[must_use]
    pub const fn host_secrets_and_cloud_metadata_unreachable(&self) -> bool {
        self.isolation.network_devices == 0 && self.isolation.host_directory_shares == 0
    }
}

fn string_field(value: &serde_yaml::Value, key: &str) -> Result<String, CapsuleError> {
    value
        .get(key)
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| CapsuleError::CatalogInvalid(format!("missing {key}")))
}

fn string_list(value: &serde_yaml::Value, key: &str) -> Result<Vec<String>, CapsuleError> {
    value
        .get(key)
        .and_then(serde_yaml::Value::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .ok_or_else(|| CapsuleError::CatalogInvalid(format!("missing {key} list")))
}

fn u32_field(value: &serde_yaml::Value, key: &str) -> Result<u32, CapsuleError> {
    value
        .get(key)
        .and_then(serde_yaml::Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| CapsuleError::CatalogInvalid(format!("missing {key}")))
}

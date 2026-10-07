# Quarterly operations review

This report covers **three** areas: *reliability*, throughput and cost. See [the dashboard](https://dash.example.com/ops) for live figures.

## Reliability

- Availability held at 99.95 percent
- Two incidents, both resolved within the hour
  - the cache outage
  - the failed deploy

1. Review the postmortems
2. Close the open actions

> A system is reliable when its users stop thinking about it.

## Numbers

| Region | Revenue | Churn |
| --- | ---: | ---: |
| North | 12 | 1.9 |
| South | 9 | 2.4 |

### Snippet

```sh
make release && make verify
```

Inline `code` and a raw <b>tag</b> that must stay text.

---

A final paragraph with an escaped \* star and an image:

![A single white pixel](data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==)

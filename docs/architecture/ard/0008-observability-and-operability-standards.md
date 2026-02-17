# ARD-0008: Observability and Operability Standards

- Version: 1.0
- Date: 2026-02-15
- Applies to: logging/metrics/tracing in `sof-observer`

## Goals

- Reliability: faster detection and diagnosis of failures.
- Efficiency: low-overhead telemetry.
- Speed: minimal impact on hot paths.
- Maintainability: consistent operational signals across modules.

## Logging

- Use structured logs with stable field names.
- Include slice, slot, index, source, and error class where relevant.
- Avoid verbose logs in hot loops unless sampled.

## Metrics

- Track throughput, parse failure rate, queue depth, reassembly completion latency, and drop rates.
- Define metric names and labels with bounded cardinality.
- Avoid high-cardinality labels that increase cost and reduce utility.

## Tracing

- Use spans for cross-component flows at infra boundaries.
- Keep trace events lightweight in critical loops.

## Operational policy

- Define SLO-aligned alerts for critical failure and latency conditions.
- Distinguish noisy diagnostics from actionable alerts.
- Every alert must have an owner and a runbook reference.

## Exit criteria

1. New features include required logs/metrics/traces.
2. Telemetry overhead is reviewed for hot paths.
3. Alerting and runbook updates ship with operationally significant changes.

# Operations

These pages are about keeping SOF useful under real traffic, not just making examples compile.

If SOF is still new, start with:

1. [Before You Start](../getting-started/before-you-start.md)
2. [Common Questions](../getting-started/common-questions.md)
3. [First Runtime Bring-Up](../getting-started/first-runtime.md)

## Operating Principles

- start from safe defaults
- measure before changing queue or thread counts
- treat ingress choice as the first performance decision, not a late tuning detail
- treat outbound relay and repair traffic as an explicit posture decision
- prefer typed tuning profiles over copying environment bundles
- document host-specific changes so they are reproducible

## Read This Section For

- choosing the right deployment mode
- understanding SOF's relay and repair behavior
- understanding what SOF can and cannot change about latency
- finding the safe baseline before expert-only tuning
- looking up one specific runtime knob quickly in the [Knob Registry](knob-registry.md)

The main split in this section is:

- [Deployment Modes](deployment-modes.md) for mode and trust posture
- [Tuning and Environment Controls](tuning-and-env.md) for what to change only after measurement

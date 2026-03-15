# Operations

The operations material is written for people who have to keep SOF useful under real traffic rather
than just make examples compile.

This section is part of the external user track. It focuses on deployment posture, traffic
behavior, and safe tuning order rather than repository contribution rules.

If you are new to SOF, do not start here first. Read:

1. [Before You Start](../getting-started/before-you-start.md)
2. [Common Questions](../getting-started/common-questions.md)
3. [First Runtime Bring-Up](../getting-started/first-runtime.md)

## Operating Principles

- start from safe defaults
- measure before changing queue or thread counts
- treat outbound relay and repair traffic as an explicit posture decision
- prefer typed tuning profiles over copying environment bundles
- document host-specific changes so they are reproducible

## Read This Section For

- choosing the right deployment mode
- understanding SOF's relay and repair behavior
- finding the safe baseline before expert-only tuning
- looking up one specific runtime knob quickly in the [Knob Registry](knob-registry.md)

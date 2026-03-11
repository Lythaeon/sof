# sof-tx fuzzing

This package contains focused fuzz targets for `sof-tx`.

Current targets:

- `submit_jito_mode`: fuzzes the `JitoOnly` submit path through `TxSubmitClient`
- `submit_rpc_only`: fuzzes the `RpcOnly` submit path
- `submit_direct_only`: fuzzes the `DirectOnly` submit path
- `submit_hybrid`: fuzzes the `Hybrid` submit path
- `routing_select_targets`: fuzzes routing target selection behavior
- `signature_deduper`: fuzzes signature dedupe window behavior
- `tx_builder_toggles`: fuzzes `TxBuilder` toggle transitions, including explicit fee reset helpers

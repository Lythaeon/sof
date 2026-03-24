//! SOF runtime example with multiple no-op plugin hook types for profiling.
#![doc(hidden)]

use async_trait::async_trait;
use sof::{
    framework::{
        AccountTouchEvent, ClusterTopologyEvent, DatasetEvent, LeaderScheduleEvent,
        ObservedRecentBlockhashEvent, Plugin, PluginConfig, PluginContext, PluginHost,
        PluginSetupError, RawPacketEvent, ReorgEvent, ShredEvent, SlotStatusEvent,
        TransactionBatchEvent, TransactionEvent, TransactionViewBatchEvent,
    },
    runtime::{ObserverRuntime, RuntimeError},
};
use thiserror::Error;

macro_rules! define_noop_plugin {
    ($name:ident, $plugin_name:literal, $config:expr, $method:ident, $event_ty:ty) => {
        #[derive(Debug, Clone, Copy, Default)]
        struct $name;

        #[async_trait]
        impl Plugin for $name {
            fn name(&self) -> &'static str {
                $plugin_name
            }

            fn config(&self) -> PluginConfig {
                $config
            }

            async fn setup(&self, _ctx: PluginContext) -> Result<(), PluginSetupError> {
                Ok(())
            }

            async fn $method(&self, _event: $event_ty) {}
        }
    };
}

define_noop_plugin!(
    RawPacketPlugin,
    "noop-raw-packet",
    PluginConfig::new().with_raw_packet(),
    on_raw_packet,
    RawPacketEvent
);
define_noop_plugin!(
    ShredPlugin,
    "noop-shred",
    PluginConfig::new().with_shred(),
    on_shred,
    ShredEvent
);
define_noop_plugin!(
    InlineTransactionPlugin,
    "noop-inline-transaction",
    PluginConfig::new().with_inline_transaction(),
    on_transaction,
    &TransactionEvent
);
define_noop_plugin!(
    StandardTransactionPlugin,
    "noop-standard-transaction",
    PluginConfig::new().with_transaction(),
    on_transaction,
    &TransactionEvent
);
define_noop_plugin!(
    DatasetPlugin,
    "noop-dataset",
    PluginConfig::new().with_dataset(),
    on_dataset,
    DatasetEvent
);
define_noop_plugin!(
    TransactionBatchPlugin,
    "noop-transaction-batch",
    PluginConfig::new().with_transaction_batch(),
    on_transaction_batch,
    &TransactionBatchEvent
);
define_noop_plugin!(
    TransactionViewBatchPlugin,
    "noop-transaction-view-batch",
    PluginConfig::new().with_transaction_view_batch(),
    on_transaction_view_batch,
    &TransactionViewBatchEvent
);
define_noop_plugin!(
    AccountTouchPlugin,
    "noop-account-touch",
    PluginConfig::new().with_account_touch(),
    on_account_touch,
    &AccountTouchEvent
);
define_noop_plugin!(
    SlotStatusPlugin,
    "noop-slot-status",
    PluginConfig::new().with_slot_status(),
    on_slot_status,
    SlotStatusEvent
);
define_noop_plugin!(
    ReorgPlugin,
    "noop-reorg",
    PluginConfig::new().with_reorg(),
    on_reorg,
    ReorgEvent
);
define_noop_plugin!(
    RecentBlockhashPlugin,
    "noop-recent-blockhash",
    PluginConfig::new().with_recent_blockhash(),
    on_recent_blockhash,
    ObservedRecentBlockhashEvent
);
define_noop_plugin!(
    ClusterTopologyPlugin,
    "noop-cluster-topology",
    PluginConfig::new().with_cluster_topology(),
    on_cluster_topology,
    ClusterTopologyEvent
);
define_noop_plugin!(
    LeaderSchedulePlugin,
    "noop-leader-schedule",
    PluginConfig::new().with_leader_schedule(),
    on_leader_schedule,
    LeaderScheduleEvent
);

#[derive(Debug, Error)]
enum ObserverMultiHookNoopError {
    #[error("examples are release-only; run with `{command}`")]
    ReleaseModeRequired { command: &'static str },
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

const fn require_release_mode() -> Result<(), ObserverMultiHookNoopError> {
    if cfg!(debug_assertions) {
        return Err(ObserverMultiHookNoopError::ReleaseModeRequired {
            command: "cargo run --release -p sof --example observer_multi_hook_noop",
        });
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), ObserverMultiHookNoopError> {
    require_release_mode()?;

    let host = PluginHost::builder()
        .add_plugin(RawPacketPlugin)
        .add_plugin(ShredPlugin)
        .add_plugin(InlineTransactionPlugin)
        .add_plugin(StandardTransactionPlugin)
        .add_plugin(DatasetPlugin)
        .add_plugin(TransactionBatchPlugin)
        .add_plugin(TransactionViewBatchPlugin)
        .add_plugin(AccountTouchPlugin)
        .add_plugin(SlotStatusPlugin)
        .add_plugin(ReorgPlugin)
        .add_plugin(RecentBlockhashPlugin)
        .add_plugin(ClusterTopologyPlugin)
        .add_plugin(LeaderSchedulePlugin)
        .build();

    tracing::info!(
        plugins = ?host.plugin_names(),
        "starting SOF multi-hook noop observer"
    );
    ObserverRuntime::new()
        .with_plugin_host(host)
        .run_until_termination_signal()
        .await?;
    Ok(())
}

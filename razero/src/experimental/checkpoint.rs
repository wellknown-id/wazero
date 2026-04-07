pub use super::snapshotter::{get_snapshotter, with_snapshotter, Snapshot, Snapshotter};

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::{
        api::wasm::ValueType,
        ctx_keys::Context,
        experimental::{
            checkpoint::{get_snapshotter, with_snapshotter},
            snapshotter::{Snapshot, Snapshotter},
        },
        ModuleConfig, Runtime,
    };

    const SNAPSHOT_EXAMPLE_WASM: &[u8] =
        include_bytes!("../../../experimental/testdata/snapshot.wasm");

    #[test]
    fn checkpoint_apis_alias_snapshotter() {
        let ctx = with_snapshotter(&Context::default());
        assert!(get_snapshotter(&ctx).is_none());
        assert!(ctx.snapshotter_enabled);
        let _ = Option::<&dyn Snapshotter>::None;
    }

    #[test]
    fn checkpoint_aliases_enable_snapshot_restore_behavior() {
        let runtime = Runtime::new();
        let snapshot = Arc::new(Mutex::new(None::<Snapshot>));
        let module = runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_callback(
                {
                    let snapshot = snapshot.clone();
                    move |ctx, module, _params| {
                        *snapshot.lock().expect("snapshot poisoned") = Some(
                            get_snapshotter(&ctx)
                                .expect("snapshotter should be injected")
                                .snapshot(),
                        );

                        let restored = module
                            .exported_function("restore")
                            .expect("restore export should exist")
                            .call_with_context(&ctx, &[])?;
                        assert_eq!(vec![0], restored);
                        Ok(vec![2])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .export("snapshot")
            .new_function_builder()
            .with_callback(
                {
                    let snapshot = snapshot.clone();
                    move |_ctx, _module, _params| {
                        snapshot
                            .lock()
                            .expect("snapshot poisoned")
                            .as_ref()
                            .expect("snapshot should be present")
                            .restore(&[12]);
                        Ok(vec![0])
                    }
                },
                &[],
                &[ValueType::I32],
            )
            .export("restore")
            .instantiate(&Context::default())
            .unwrap();

        let results = module
            .exported_function("snapshot")
            .unwrap()
            .call_with_context(&with_snapshotter(&Context::default()), &[])
            .unwrap();
        assert_eq!(vec![12], results);
    }

    #[test]
    fn checkpoint_aliases_support_go_snapshot_example_flow() {
        let runtime = Runtime::new();
        let snapshots = Arc::new(Mutex::new(Vec::<Snapshot>::new()));

        runtime
            .new_host_module_builder("example")
            .new_function_builder()
            .with_callback(
                {
                    let snapshots = snapshots.clone();
                    move |ctx, module, params| {
                        let snapshot = get_snapshotter(&ctx)
                            .expect("snapshotter should be injected")
                            .snapshot();
                        let mut snapshots = snapshots.lock().expect("snapshots poisoned");
                        let idx = snapshots.len() as u32;
                        snapshots.push(snapshot);
                        assert!(module
                            .memory()
                            .expect("guest memory should be present")
                            .write_u32_le(params[0] as u32, idx));
                        Ok(vec![0])
                    }
                },
                &[ValueType::I32],
                &[ValueType::I32],
            )
            .export("snapshot")
            .new_function_builder()
            .with_callback(
                {
                    let snapshots = snapshots.clone();
                    move |_ctx, module, params| {
                        let idx = module
                            .memory()
                            .expect("guest memory should be present")
                            .read_u32_le(params[0] as u32)
                            .expect("snapshot index should be written")
                            as usize;
                        snapshots.lock().expect("snapshots poisoned")[idx].restore(&[5]);
                        Ok(Vec::new())
                    }
                },
                &[ValueType::I32],
                &[],
            )
            .export("restore")
            .instantiate(&Context::default())
            .unwrap();

        let guest = runtime
            .instantiate_binary(SNAPSHOT_EXAMPLE_WASM, ModuleConfig::new())
            .unwrap();

        let results = guest
            .exported_function("run")
            .unwrap()
            .call_with_context(&with_snapshotter(&Context::default()), &[])
            .unwrap();

        assert_eq!(vec![5], results);
        assert_eq!(Some(0), guest.memory().unwrap().read_u32_le(0));
        assert_eq!(1, snapshots.lock().expect("snapshots poisoned").len());
    }
}

---
id: gpui-tokio-runtime-bridging
title: Bridging GPUI's executors and birdman-imap's Tokio runtime
altitude: 2
topics:
- ui
relations:
- type: part_of
  target: gpui-application
- type: depends_on
  target: imap-sync-engine
summary: 'The established nesting for user-triggered IMAP work: cx.spawn wrapping runtime.spawn wrapping with_timeout, and the double-Result it produces.'
---

# Bridging GPUI's executors and birdman-imap's Tokio runtime

GPUI's executors are not Tokio, and `birdman-imap`'s IMAP code requires Tokio.
`EngineHandle::runtime` (a `tokio::runtime::Handle`) is the bridge —
`Handle::spawn` is safe to call from any thread and schedules onto birdman-imap's
runtime regardless of the caller's own context.

## The established shape

Every user-triggered IMAP operation in `crates/birdman-ui/src/state.rs` —
`select_message`, `toggle_flag`, `delete_selected`, `sync_now` — uses the same
nesting. Match it rather than inventing a variant:

```rust
cx.spawn(async move |this, cx| {
    let result: Result<Result<(), CoreError>, _> = runtime
        .spawn(async move {
            birdman_imap::with_timeout(async move {
                let mut session = session_cache.selected(&account, &credentials, &folder.imap_path).await?;
                let result = /* ... IMAP work ... */;
                if result.is_err() { session.invalidate(); }
                result
            }).await
        })
        .await;
    // classify, then apply on the foreground
    let _ = this.update(cx, |state, cx| { /* mutate AppState */ cx.notify(); });
})
.detach();
```

Layer by layer:

- `cx.spawn` — a **foreground** GPUI task, so `this.update` can touch `AppState`.
- `runtime.spawn` — hops onto birdman-imap's Tokio runtime, where async-imap works.
- `with_timeout` — caps it at `ON_DEMAND_TIMEOUT` (20s).
- `session_cache.selected(...)` + `invalidate()` on error — see
  [[on-demand-imap-session-cache]].
- `.detach()` — nothing awaits these tasks.

## The double `Result` is real

The outer `Result` is Tokio's `JoinError` (the task panicked); the inner is
`CoreError`. Both must be handled. The two idioms in use:

```rust
let error_message = match &result {
    Ok(Ok(())) => None,
    Ok(Err(err)) => Some(err.to_string()),
    Err(_) => Some("internal task error".to_string()),
};
```

and the terser `result.unwrap_or(Err(CoreError::CredentialTaskPanicked))`.

## `cx.spawn` vs. `cx.background_spawn`

- `cx.spawn` — foreground; can call `this.update` to mutate an entity. Used for
  everything that awaits I/O and then updates state.
- `cx.background_spawn` — a real background thread for CPU work, and it *cannot*
  update entities. Used only for HTML rendering (see
  the reading pane), whose result is then applied by an enclosing
  `cx.spawn`.

## Cloning before the move

`Arc`s and handles (`credentials`, `session_cache`, `store`, `runtime`) are
cloned into locals immediately before `cx.spawn`, because the async block is
`move` and can't borrow `self`. `msg.uid` and `msg.flags` are copied out for the
same reason. This is why these functions all start with a block of `let x =
self.x.clone();` lines.

/// Version of the contract the core exposes to the plugins' UI modules
/// (`vue` + `@ritornello/ui`). A plugin module exports its own `contract`;
/// the shell refuses to mount it on a mismatch, with an explicit message.
/// Bump on every incompatible change of the kit.
///
/// `ReorderList` / `ReorderHandle` were added **without** bumping it, by the
/// owner's decision, for this once. The cost is accepted knowingly: a plugin
/// `ui.js` built against this kit (files, radio, web-streaming) fails to load
/// on an older core -- a core rolled back alone, a partial update -- and its
/// page shows "unavailable" rather than the contract-mismatch message. The
/// next incompatible change of the kit bumps the number as usual.
export const UI_CONTRACT = 1

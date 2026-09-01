/// Version of the contract the core exposes to the plugins' UI modules
/// (`vue` + `@ritornello/ui`). A plugin module exports its own `contract`;
/// the shell refuses to mount it on a mismatch, with an explicit message.
/// Bump on every incompatible change of the kit.
export const UI_CONTRACT = 1

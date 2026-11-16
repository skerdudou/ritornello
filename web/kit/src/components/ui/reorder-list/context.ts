import type { InjectionKey, Ref } from 'vue'

/**
 * Whether the enclosing `ReorderList` refuses a drag -- disabled by its
 * parent, or waiting for the answer to a drop -- for its handles. Injected
 * rather than passed as a prop so a consumer cannot forget it on one row and
 * leave a handle that looks grabbable on a list that will not move.
 */
export const REORDER_DISABLED: InjectionKey<Readonly<Ref<boolean>>> = Symbol('reorder-disabled')

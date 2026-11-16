import type { InjectionKey, Ref } from 'vue'

/**
 * The enclosing `ReorderList`'s `disabled`, for its handles. Injected rather
 * than passed as a prop so a consumer cannot forget it on one row and leave a
 * handle that looks grabbable on a list that will not move.
 */
export const REORDER_DISABLED: InjectionKey<Ref<boolean>> = Symbol('reorder-disabled')

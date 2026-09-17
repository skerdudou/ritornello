// Reordering by drag-and-drop: pure logic, testable without a DOM.
//
// Shared by every table in this repository that lets an operator drag a row:
// the radio station list keeps the preset **as** the row's position (see
// `save()` in `RadioAdmin.vue` and `save_numbers_from_1_to_n_by_position` on
// the plugin side), and the configuration page's plugins table sends the
// result straight to `POST /api/plugins/{name}/move`. Two implementations of
// one reordering on the same configuration page would be two behaviours to
// keep in step by hand; this is the one the kit owns instead.

/**
 * Moves the element at index `from` to index `to`, returning a **new**
 * list.
 *
 * An out-of-bounds index or a move to the same spot leaves the list
 * unchanged rather than throwing: the indices come from the browser's
 * drag-and-drop events, where a target can disappear between `dragstart`
 * and `drop`.
 */
export function move<T>(list: readonly T[], from: number, to: number): T[] {
  const copy = [...list]
  if (
    !Number.isInteger(from) ||
    !Number.isInteger(to) ||
    from < 0 ||
    to < 0 ||
    from >= copy.length ||
    to >= copy.length ||
    from === to
  ) {
    return copy
  }
  const [element] = copy.splice(from, 1)
  // `element` cannot be `undefined` here (index checked above), but
  // `splice`'s type does not know that.
  copy.splice(to, 0, element as T)
  return copy
}

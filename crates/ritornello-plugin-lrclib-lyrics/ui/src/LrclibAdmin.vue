<script setup lang="ts">
import { api, Button, Card, CardContent, CardHeader, CardTitle, createT, Skeleton, toast, useSkeleton, type Catalog } from '@ritornello/ui'
import { computed, onMounted, ref } from 'vue'

// `base` is part of the plugin UI contract, like `catalog`: the absolute
// prefix under which the core serves this plugin's routes, provided by the
// shell. Required, no default: the name comes from `plugins.toml`, and a
// default would be silently wrong under another name.
const props = defineProps<{ catalog: Catalog; base: string }>()
const t = computed(() => createT(props.catalog))

function url(path: string): string {
  return `${props.base}${path}`
}

/** What `get_data` answers: the cache's counts, from its in-memory index. */
interface Stats {
  bytes: number
  found: number
  absent: number
}

const stats = ref<Stats | null>(null)

// Nothing for the first fraction of a second, a placeholder only if the wait
// outlasts it: the kit's rhythm, shared with the shell and the other plugins.
const skeleton = useSkeleton(() => stats.value === null)

async function reload(): Promise<void> {
  try {
    stats.value = await api.get<Stats>(url('api/data'))
  } catch (e) {
    // No catalog key covers a load failure: the request's own message is the
    // only text there is, as on the other plugins' pages.
    toast.error((e as Error).message)
  }
}

onMounted(reload)

/**
 * The size, in the largest unit that keeps it above one: a cache holds from
 * a few hundred bytes to its 20 MiB ceiling. Binary units, as everywhere
 * else in the interface (the cover cache speaks MiB and KiB).
 *
 * Three literal calls rather than one key built from the unit's name:
 * `i18nKeysUsed.test.ts` only sees keys written out in full.
 */
const size = computed(() => {
  const bytes = stats.value?.bytes ?? 0
  if (bytes < 1024) return t.value('size_bytes', { n: bytes })
  if (bytes < 1024 * 1024) return t.value('size_kib', { n: (bytes / 1024).toFixed(1) })
  return t.value('size_mib', { n: (bytes / (1024 * 1024)).toFixed(1) })
})

async function clear(): Promise<void> {
  const err = await api.put(url('api/data'), { action: 'clear' })
  if (err) {
    // Already a translated sentence, resolved by the core from this
    // plugin's catalog.
    toast.error(err)
  }
  // Reloaded even on a refusal: a clear that failed halfway has still
  // removed some entries, and the page must show what is left.
  await reload()
}
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t('title') }}</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <p class="text-sm text-muted-foreground">{{ t('intro') }}</p>

      <div v-if="skeleton" role="status" class="space-y-2">
        <span class="sr-only">{{ t('loading') }}</span>
        <Skeleton class="h-5 w-48" aria-hidden="true" />
        <Skeleton class="h-5 w-40" aria-hidden="true" />
        <Skeleton class="h-5 w-40" aria-hidden="true" />
      </div>

      <dl v-else-if="stats" class="grid grid-cols-[auto_1fr] gap-x-6 gap-y-1 text-sm">
        <dt class="text-muted-foreground">{{ t('size') }}</dt>
        <dd data-lyrics-cache-size class="tabular-nums">{{ size }}</dd>
        <dt class="text-muted-foreground">{{ t('found') }}</dt>
        <dd data-lyrics-cache-found class="tabular-nums">{{ stats.found }}</dd>
        <dt class="text-muted-foreground">{{ t('absent') }}</dt>
        <dd data-lyrics-cache-absent class="tabular-nums">{{ stats.absent }}</dd>
      </dl>

      <Button variant="secondary" data-lyrics-cache-clear @click="clear">{{ t('clear') }}</Button>

      <!-- LRCLIB asks for no attribution in its documentation (checked on
           2026-11-03); naming the source of the words is a courtesy, and
           says where to add the lyrics it lacks. -->
      <p class="text-xs text-muted-foreground">
        {{ t('attribution') }}
        <a href="https://lrclib.net" target="_blank" rel="noopener noreferrer" class="underline">lrclib.net</a>
      </p>
    </CardContent>
  </Card>
</template>

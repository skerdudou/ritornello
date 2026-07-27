import { describe, expect, it } from 'vitest'
import { router } from './router'

describe('router', () => {
  it('conserve les URL historiques', async () => {
    await router.push('/')
    expect(router.currentRoute.value.name).toBe('home')
    await router.push('/status')
    expect(router.currentRoute.value.name).toBe('status')
    await router.push('/plugins/radio/')
    expect(router.currentRoute.value.name).toBe('plugin')
    expect(router.currentRoute.value.params.name).toBe('radio')
    // La forme canonique ne bouge pas : c'est un invariant epingle par ailleurs
    // cote coeur (`serves_shell("/plugins/radio/")`).
    expect(router.currentRoute.value.fullPath).toBe('/plugins/radio/')
  })

  it('redirige la forme sans slash final vers la forme canonique', async () => {
    // IMPORTANT 6 de la revue finale. `/plugins/radio` et `/plugins/radio/`
    // matchaient tous deux la route du plugin (le routeur n'est pas strict par
    // defaut) : la page se montait sur la forme sans slash, mais les modules
    // resolvaient alors `./api/data` vers `/plugins/api/data` — que le coeur
    // interprete comme le plugin « api » -> 404, table vide et tous les boutons
    // en echec. La prop `base` supprime la dependance a la forme de l'URL ; on
    // canonise en plus l'URL pour ne pas laisser vivre deux formes.
    await router.push('/plugins/generic-input')
    expect(router.currentRoute.value.fullPath).toBe('/plugins/generic-input/')
    expect(router.currentRoute.value.name).toBe('plugin')
    expect(router.currentRoute.value.params.name).toBe('generic-input')
  })
})

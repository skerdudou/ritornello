import { flushPromises, mount } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import FilesAdmin from './FilesAdmin.vue'
import { BASE, CATALOGUE, monter, serveur } from './harnais'

const NAS = {
  name: 'nas',
  kind: 'smb',
  host: 'nas.local',
  share: 'musique',
  subpath: 'Albums',
  user: 'steven',
  domain: '',
  writable: false,
  mounted: true,
}

describe('volet des racines', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(() => vi.useRealTimers())

  it('liste les racines déclarées avec leur cible et l’état du montage', async () => {
    const { w } = await monter({ roots: [NAS] })
    expect(w.find('[data-root-target]').text()).toBe('//nas.local/musique/Albums')
    expect(w.find('[data-root-mounted]').text()).toBe('monté')
    expect(w.find('[data-root-kind]').text()).toBe('partage réseau')
  })

  it('dit qu’un mot de passe laissé vide conserve celui qui est enregistré', async () => {
    // Régression encodée : `api/data` ne rend **jamais** le mot de passe, donc
    // le champ repart toujours vide. Sans cette phrase, l'utilisateur lit un
    // champ vide comme un mot de passe effacé — il le retape à chaque
    // enregistrement, ou croit l'avoir perdu.
    const { w, s } = await monter({ roots: [NAS] })
    expect(w.find('[data-password-hint]').text()).toBe(
      'Laisser vide conserve le mot de passe enregistré.',
    )
    expect((w.find('[data-root-password]').element as HTMLInputElement).value).toBe('')

    await w.find('[data-save-roots]').trigger('click')
    await flushPromises()
    // Et ce que la page envoie est bien la **chaîne vide** : c'est ce que
    // `RootInput::password` documente comme « garde celui déjà enregistré ».
    // Un `null` ou un champ absent serait une seconde convention à tenir des
    // deux côtés, pour dire la même chose.
    //
    // `path` et `subpath`, eux, partent bien à `null` quand ils sont vides :
    // ce sont des `Option<String>` côté plugin, et `Some("")` n'est pas
    // « pas de sous-chemin » — `Roots::validate` le refuse.
    expect(s.putsDe('save_roots')[0]!.roots).toEqual([
      {
        name: 'nas',
        kind: 'smb',
        path: null,
        host: 'nas.local',
        share: 'musique',
        subpath: 'Albums',
        user: 'steven',
        domain: '',
        writable: false,
        password: '',
      },
    ])
  })

  it('n’affiche pas la phrase du mot de passe quand aucune racine n’est un partage', async () => {
    const { w } = await monter({ roots: [{ name: 'usb', kind: 'local', path: '/mnt/usb' }] })
    expect(w.find('[data-password-hint]').exists()).toBe(false)
    expect(w.find('[data-root-path]').exists()).toBe(true)
    expect(w.find('[data-root-host]').exists()).toBe(false)
  })

  it('déclare un partage saisi de bout en bout', async () => {
    const { w, s } = await monter()
    expect(w.find('[data-no-roots]').exists()).toBe(true)
    await w.find('[data-add-share]').trigger('click')
    await w.find('[data-root-name]').setValue('nas')
    await w.find('[data-root-host]').setValue('nas.local')
    await w.find('[data-root-share]').setValue('musique')
    await w.find('[data-root-subpath]').setValue('Albums')
    await w.find('[data-root-user]').setValue('steven')
    await w.find('[data-root-password]').setValue('secret')
    await w.find('[data-root-writable]').setValue(true)
    await w.find('[data-save-roots]').trigger('click')
    await flushPromises()
    expect(s.putsDe('save_roots')[0]!.roots).toEqual([
      {
        name: 'nas',
        kind: 'smb',
        path: null,
        host: 'nas.local',
        share: 'musique',
        subpath: 'Albums',
        user: 'steven',
        domain: '',
        writable: true,
        password: 'secret',
      },
    ])
  })

  it('ajoute un dossier local avec son chemin absolu', async () => {
    const { w, s } = await monter()
    await w.find('[data-add-local]').trigger('click')
    await w.find('[data-root-name]').setValue('usb')
    await w.find('[data-root-path]').setValue('/mnt/usb')
    await w.find('[data-save-roots]').trigger('click')
    await flushPromises()
    const roots = s.putsDe('save_roots')[0]!.roots as Record<string, unknown>[]
    expect(roots[0]!.kind).toBe('local')
    expect(roots[0]!.path).toBe('/mnt/usb')
    // Régression encodée : un `subpath` à `""` deviendrait `Some("")` côté
    // plugin, que `Roots::validate` refuse (`champ_sur` rejette la chaîne
    // vide) — la racine serait impossible à enregistrer sans qu'aucun champ
    // visible n'ait l'air fautif.
    expect(roots[0]!.subpath).toBeNull()
  })

  it('retirer une racine la fait disparaître de la charge enregistrée', async () => {
    const { w, s } = await monter({
      roots: [NAS, { name: 'usb', kind: 'local', path: '/mnt/usb' }],
    })
    expect(w.findAll('[data-root]')).toHaveLength(2)
    await w.findAll('[data-root-remove]')[0]!.trigger('click')
    await w.find('[data-save-roots]').trigger('click')
    await flushPromises()
    const roots = s.putsDe('save_roots')[0]!.roots as Record<string, unknown>[]
    expect(roots.map((r) => r.name)).toEqual(['usb'])
  })

  it('le sondage d’un balayage n’écrase pas la saisie en cours', async () => {
    // Régression encodée : pendant un `add_dir`, la page redemande `api/data`
    // chaque seconde. Une resynchronisation naïve du formulaire à chaque
    // réponse ferait disparaître sous les doigts de l'utilisateur le mot de
    // passe qu'il est en train de taper.
    vi.useFakeTimers({ shouldAdvanceTime: true })
    const s = serveur({ roots: [NAS], scan: { running: true, found: 3, dir: 'Albums' } })
    const w = mount(FilesAdmin, { props: { catalog: CATALOGUE, base: BASE } })
    await flushPromises()
    await w.find('[data-root-password]').setValue('secret')
    s.data.scan = { running: true, found: 40, dir: 'Albums/Jazz' }
    vi.advanceTimersByTime(1000)
    await flushPromises()
    expect((w.find('[data-root-password]').element as HTMLInputElement).value).toBe('secret')
    // Le reste de la page, lui, a bien suivi.
    expect(w.find('[data-scan]').text()).toContain('40')
  })

  it('demande la réconciliation des montages sans autre charge', async () => {
    const { w, s } = await monter({ roots: [NAS] })
    await w.find('[data-mount]').trigger('click')
    await flushPromises()
    expect(s.putsDe('mount')).toEqual([{ op: 'mount' }])
  })
})

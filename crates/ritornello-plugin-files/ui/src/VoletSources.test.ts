import { flushPromises } from '@vue/test-utils'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { dansPopin, monter, nettoyerPopins } from './harnais'

const USB = {
  name: 'usb',
  kind: 'local',
  path: '/media/usb',
  host: '',
  share: '',
  user: '',
  domain: '',
  writable: false,
  mounted: true,
}

const NAS = {
  name: 'musique',
  kind: 'smb',
  host: '192.168.1.15',
  share: 'music',
  subpath: 'Yann Tiersen',
  user: 'ritornello',
  domain: '',
  writable: false,
  mounted: true,
}

describe('volet des sources', () => {
  beforeEach(() => vi.unstubAllGlobals())
  afterEach(nettoyerPopins)

  it('sans source, invite à en ajouter une plutôt que de laisser un vide', async () => {
    const { w } = await monter()
    expect(w.find('[data-no-sources]').exists()).toBe(true)
    expect(w.findAll('[data-source]')).toHaveLength(0)
  })

  it('affiche la cible d’une source et l’état observé de son montage', async () => {
    const { w } = await monter({ roots: [NAS] })
    expect(w.find('[data-source-target]').text()).toBe('//192.168.1.15/music/Yann Tiersen')
    expect(w.find('[data-source-mounted]').text()).toBe('monté')
    expect(w.find('[data-source-kind]').text()).toBe('partage réseau')
  })

  it('ajoute toute une source à la liste en un clic', async () => {
    // La demande explicite : depuis les sources déclarées, « tout ajouter »
    // doit être à portée immédiate. Le chemin vide désigne la source entière.
    const { w, s } = await monter({ roots: [USB] })
    await w.find('[data-add-all]').trigger('click')
    await flushPromises()
    expect(s.putsDe('add_dir')[0]).toEqual({ op: 'add_dir', root: 'usb', path: '' })
  })

  it('retire une source en la nommant', async () => {
    const { w, s } = await monter({ roots: [USB] })
    await w.find('[data-remove-source]').trigger('click')
    await flushPromises()
    expect(s.putsDe('remove_source')[0]).toEqual({ op: 'remove_source', name: 'usb' })
  })

  it('bascule l’inscriptibilité sans repasser par une redéclaration', async () => {
    // Opération à part, et c'est le point : sans elle, changer d'avis
    // imposerait de retirer puis redéclarer la source, donc de resaisir le mot
    // de passe que la page ne connaît pas.
    const { w, s } = await monter({ roots: [NAS] })
    const cocher = w.find('[data-writable]')
    await cocher.setValue(true)
    await flushPromises()
    expect(s.putsDe('set_writable')[0]).toEqual({
      op: 'set_writable',
      name: 'musique',
      writable: true,
    })
  })

  it('n’offre l’inscriptibilité que sur un partage', async () => {
    // Un dossier de l'appareil est inscriptible ou non selon le système de
    // fichiers ; l'interrupteur ne pilote que les options de montage cifs, il
    // ne voudrait rien dire ici.
    const { w } = await monter({ roots: [USB] })
    expect(w.find('[data-writable]').exists()).toBe(false)
    expect(w.find('[data-source-mounted]').exists()).toBe(false)
  })

  it('montre un échec de montage et permet de le réessayer', async () => {
    // Le montage suit la déclaration : sans ce rapport, une source resterait
    // « non montée » sans que rien ne dise pourquoi.
    const { w, s } = await monter({
      roots: [{ ...NAS, mounted: false }],
      mount_error: 'Interactive authentication required.',
    })
    expect(w.find('[data-mount-error]').text()).toContain('Interactive authentication required.')
    await w.find('[data-retry-mount]').trigger('click')
    await flushPromises()
    expect(s.putsDe('mount')).toHaveLength(1)
  })

  it('ne montre aucun bandeau de montage quand tout va bien', async () => {
    const { w } = await monter({ roots: [NAS] })
    expect(w.find('[data-mount-error]').exists()).toBe(false)
  })

  it('ouvre l’assistant de l’appareil en prévenant le plugin', async () => {
    // L'ouverture passe par le plugin et pas seulement par un booléen local :
    // c'est lui qui porte l'état de l'assistant, et une popin qui s'afficherait
    // sans le prévenir hériterait de celui de la précédente.
    const { w, s } = await monter({ volumes: [{ path: '/media/usb', fstype: 'vfat' }] })
    await w.find('[data-add-device]').trigger('click')
    await flushPromises()
    expect(s.putsDe('explore_open')[0]).toEqual({ op: 'explore_open', kind: 'local' })
    // Le contenu de la popin vit dans un portail : `w.find` ne l'y verrait
    // jamais, quel que soit l'état du composant.
    expect(dansPopin('[data-dlg-appareil]')).not.toBeNull()
  })

  it('ouvre l’assistant réseau en prévenant le plugin', async () => {
    const { w, s } = await monter({ can_browse_smb: true })
    await w.find('[data-add-share]').trigger('click')
    await flushPromises()
    expect(s.putsDe('explore_open')[0]).toEqual({ op: 'explore_open', kind: 'smb' })
    expect(dansPopin('[data-dlg-partage]')).not.toBeNull()
  })

  it('les deux popins restent fermées tant qu’on ne les ouvre pas', async () => {
    const { w } = await monter({ roots: [USB] })
    expect(w.find('[data-volet-sources]').exists()).toBe(true)
    expect(dansPopin('[data-dlg-appareil]')).toBeNull()
    expect(dansPopin('[data-dlg-partage]')).toBeNull()
  })
})

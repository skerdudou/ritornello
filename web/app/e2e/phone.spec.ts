import { expect, test } from '@playwright/test'

test('sur téléphone : barre basse, nav du haut absente, tuile nommée', async ({ page }) => {
  await page.goto('/')
  await expect(page.locator('[data-nav-basse]')).toBeVisible()
  await expect(page.locator('[data-nav-haut]')).toBeHidden()
  await expect(page.locator('[data-nav-basse] a')).toHaveCount(4)
  // La présélection 1 (FIP, stations.toml du harness) joue et porte son nom.
  const tuile = page.locator('[data-preset-button="1"]')
  await expect(tuile).toHaveAttribute('aria-current', 'true')
  await expect(tuile.locator('[data-preset-name]')).toHaveText('FIP')
  // Le transport n'a ni ±10 s ni volume step à step, et step d'Éjecter sur la radio.
  await expect(page.locator('[data-remote-command="Eject"]')).toHaveCount(0)
  await expect(page.locator('[data-remote-command]')).toHaveCount(5) // Prev PlayPause Next Stop Mute
})

test('sur téléphone : glisser le curseur de volume envoie un SetVolume que le cœur renvoie', async ({ page }) => {
  // La preuve de bout en bout du Slider : un vrai geste tactile, une seule
  // command au relâchement, et la trame SSE qui revient avec la valeur.
  // Le volume plutôt que la progression parce que la radio n'est step
  // `seekable` : la barre y est informative, sans poignée — ce qui se vérifie
  // aussi.
  await page.goto('/')
  await expect(page.locator('[data-volume]')).not.toHaveText('')
  await expect(page.locator('[data-barre] [role="slider"]')).toHaveCount(0)
  const curseur = page.locator('[data-volume-curseur]')
  const boite = await curseur.boundingBox()
  if (!boite) throw new Error('curseur de volume invisible')
  const y = boite.y + boite.height / 2
  // L'affichage [data-volume] est optimiste (mis a jour des le valueCommit
  // local), la trame SSE ne l'est step : sans attendre la reponse du POST, la
  // lecture du flux plus bas peut arriver avant que le coeur ait vraiment
  // applique la command.
  const reponse = page.waitForResponse(
    (r) => r.url().endsWith('/api/command') && r.request().method() === 'POST',
  )
  await page.mouse.move(boite.x + boite.width * 0.5, y)
  await page.mouse.down()
  await page.mouse.move(boite.x + boite.width * 0.25, y, { steps: 5 })
  await page.mouse.up()
  expect((await reponse).status()).toBe(204)
  // Entre 20 et 30 % : la position exacte dépend du padding de la poignée.
  await expect(page.locator('[data-volume]')).toHaveText(/^(2\d|30) %$/)
  // Le 204 ne prouve que la mise en file : `command_post` envoie sur
  // `cmd_tx` et repond aussitot, avant que la loop du coeur ne traite la
  // command (appel mpv puis publication sur le canal d'state). Une seule
  // lecture SSE peut donc encore tomber sur la trame d'avant — d'ou le poll,
  // qui rouvre la connexion jusqu'a une trame a jour (chaque connexion
  // reçoit l'state courant des l'ouverture, voir journey.spec.ts).
  const lireVolumeSse = () =>
    page.evaluate(
      () =>
        new Promise<number>((resolve, reject) => {
          const flux = new EventSource('/api/player')
          const timer = setTimeout(() => { flux.close(); reject(new Error('aucune trame en 2 s')) }, 2000)
          flux.onmessage = (e) => {
            clearTimeout(timer)
            flux.close()
            resolve((JSON.parse(e.data as string) as { volume: number }).volume)
          }
        }),
    )
  let dernierVolume = -1
  await expect
    .poll(async () => {
      dernierVolume = await lireVolumeSse()
      return dernierVolume >= 20 && dernierVolume <= 30
    }, { timeout: 5000 })
    .toBe(true)
  expect(dernierVolume).toBeGreaterThanOrEqual(20)
  expect(dernierVolume).toBeLessThanOrEqual(30)
})

test('sur téléphone : l’onglet Greffons mène à la list, qui mène à la page du greffon', async ({ page }) => {
  await page.goto('/')
  await page.locator('[data-nav-plugins]').click()
  // Trois plugins à page dans le harness (radio, files, generic-input) : la list.
  await expect(page).toHaveURL(/\/plugins\/$/)
  await expect(page.locator('[data-plugins-list] a')).toHaveCount(3)
  await page.locator('[data-plugins-list] a').first().click()
  await expect(page).toHaveURL(/\/plugins\/radio\/$/)
})

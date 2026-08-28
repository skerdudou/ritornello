// Regenere docs/captures/*.png depuis un coeur en marche (voir docs/development.md).
// Les captures a la main vieillissaient a chaque chantier ; celles-ci se
// refont en une command, aux deux modes et aux deux largeurs.
import { chromium } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { resolve } from 'node:path'

const BASE = process.env.RITORNELLO_URL ?? 'http://127.0.0.1:8099'

// `../../docs/captures` n'a de sens que lance depuis `web/app` : ailleurs il
// resoudrait silencieusement vers un autre dossier (voire en dehors du
// depot) sans jamais toucher les captures reellement documentees. Mieux vaut
// fail bruyamment que d'ecrire au mauvais endroit sans un mot.
const cwd = process.cwd().replace(/\\/g, '/')
if (!cwd.endsWith('/web/app')) {
  throw new Error(`lancer ce script depuis web/app (cwd actuel : ${process.cwd()})`)
}
const OUT = resolve(process.cwd(), '../../docs/captures')
mkdirSync(OUT, { recursive: true })

// `wait` : delai avant la prise, en ms. 800 suffit partout sauf sur /system,
// dont l'usage CPU est un delta calcule dans la page et l'history une
// window glissante : ouverts depuis moins d'un cycle de rafraichissement,
// ils affichent « — » et une courbe vide.
async function capture(browser, nom, { largeur, hauteur, mode, chemin = '/', wait = 800 }) {
  const page = await browser.newPage({ viewport: { width: largeur, height: hauteur }, deviceScaleFactor: 2 })
  try {
    await page.goto(`${BASE}/`)
    await page.waitForSelector('[data-preset-button]')
    // Le mode est un reglage de l'appareil (PUT /api/theme), step du browser.
    const theme = await page.evaluate(() => fetch('/api/theme').then((r) => r.json()))
    try {
      await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), { ...theme, mode })
      await page.goto(`${BASE}${chemin}`)
      await page.waitForTimeout(wait)
      await page.screenshot({ path: resolve(OUT, `${nom}.png`), fullPage: false })
    } finally {
      // Remis dans l'state trouve meme si la capture plante en chemin : sans
      // ce `finally`, un echec au milieu du script laisserait l'appareil
      // reel dans le mode de la derniere capture tentee.
      await page.evaluate((m) => fetch('/api/theme', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(m) }), theme)
    }
  } finally {
    await page.close()
  }
}

const browser = await chromium.launch()
try {
  await capture(browser, 'accueil-clair', { largeur: 1280, hauteur: 800, mode: 'light' })
  await capture(browser, 'accueil-sombre', { largeur: 1280, hauteur: 800, mode: 'dark' })
  await capture(browser, 'accueil-phone', { largeur: 390, hauteur: 844, mode: 'light' })
  await capture(browser, 'admin-radio', { largeur: 1280, hauteur: 800, mode: 'light', chemin: '/plugins/radio/' })
  await capture(browser, 'systeme', { largeur: 1280, hauteur: 800, mode: 'light', chemin: '/system', wait: 25_000 })
} finally {
  // Sinon un browser Chromium reste open (et le process ne se termine
  // jamais) des qu'une des quatre captures echoue.
  await browser.close()
}
console.log(`captures ecrites dans ${OUT}`)

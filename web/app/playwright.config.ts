import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  // Les journey ne portent que sur des files *.spec.ts : serve.mjs et
  // teardown.mjs sont le harness, step des tests.
  testMatch: '**/*.spec.ts',
  // Un seul worker : les deux projets (bureau, phone) et les trois
  // files de spec (journey, files, phone) partagent une unique
  // instance du coeur et mutent son state serveur (theme, stations, source
  // active) — les paralleliser les entrelacerait silencieusement.
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:8099' },
  // Deux viewports, un seul cœur : les journey historiques sur bureau, et le
  // journey téléphone qui vérifie la barre basse et les curseurs au doigt.
  // `workers: 1` ci-dessus vaut pour les deux projets, pour la même raison.
  projects: [
    { name: 'bureau', use: { ...devices['Desktop Chrome'] }, testIgnore: '**/phone.spec.ts' },
    { name: 'phone', use: { ...devices['Pixel 7'] }, testMatch: '**/phone.spec.ts' },
  ],
  // Le binaire doit exister : `cargo build --workspace` fait partie de la
  // chaine de build (voir deploy/build.sh).
  webServer: {
    command: 'node e2e/serve.mjs',
    url: 'http://127.0.0.1:8099/api/status',
    reuseExistingServer: false,
    timeout: 60_000,
  },
  // Arret explicite du coeur jetable, independant du sort du process node
  // du webServer : sous Windows, Playwright termine ce process par
  // `taskkill /T /F`, qui ne tue que l'arbre Windows — step le processus
  // Linux que `wsl.exe` a lance dans la VM WSL2 (une VM legere, hors de cet
  // arbre). Voir e2e/teardown.mjs pour le detail de l'arret cote WSL.
  globalTeardown: './e2e/teardown.mjs',
})

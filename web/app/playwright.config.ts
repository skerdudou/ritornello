import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  // Les parcours ne portent que sur des fichiers *.spec.ts : serve.mjs et
  // teardown.mjs sont le harnais, pas des tests.
  testMatch: '**/*.spec.ts',
  // Un seul worker : les parcours partagent une unique instance du coeur et
  // mutent son etat serveur (theme, stations) — les paralleliser les
  // entrelacerait silencieusement. Sans consequence avec un seul fichier de
  // spec aujourd'hui, mais a garder si un second fichier apparait.
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:8099', ...devices['Desktop Chrome'] },
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
  // `taskkill /T /F`, qui ne tue que l'arbre Windows — pas le processus
  // Linux que `wsl.exe` a lance dans la VM WSL2 (une VM legere, hors de cet
  // arbre). Voir e2e/teardown.mjs pour le detail de l'arret cote WSL.
  globalTeardown: './e2e/teardown.mjs',
})

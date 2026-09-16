import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  // The journeys only cover *.spec.ts files: serve.mjs and teardown.mjs are
  // the harness, not tests.
  testMatch: '**/*.spec.ts',
  // A single worker: the two projects (desktop, phone) and the three spec
  // files (journey, files, phone) share a single instance of the core and
  // mutate its server state (theme, stations, active source) —
  // parallelizing them would interleave them silently.
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:8099' },
  // Two viewports, one core: the historical journeys on desktop, and the
  // phone journey that checks the bottom bar and the finger-driven sliders.
  // `workers: 1` above applies to both projects, for the same reason.
  //
  // A third project, `scrollbars`, differs from `bureau` by a single launch
  // argument. Playwright's default headless arguments include
  // `--hide-scrollbars`, which gives the page zero-width scrollbars:
  // `innerWidth - clientWidth` is then 0 in every journey, and anything that
  // depends on a scrollbar taking layout space cannot be measured at all —
  // which is exactly what `dropdown-width.spec.ts` measures. Dropping that one
  // argument gives it the space-taking scrollbars a desktop browser really
  // draws. It is a project and not a `test.use` in the spec on purpose:
  // `test.use({ launchOptions })` *replaces* the whole object, so it would
  // also discard any other launch option the config comes to need.
  projects: [
    {
      name: 'bureau',
      use: { ...devices['Desktop Chrome'] },
      testIgnore: ['**/phone.spec.ts', '**/dropdown-width.spec.ts'],
    },
    { name: 'phone', use: { ...devices['Pixel 7'] }, testMatch: '**/phone.spec.ts' },
    {
      name: 'scrollbars',
      use: {
        ...devices['Desktop Chrome'],
        launchOptions: { ignoreDefaultArgs: ['--hide-scrollbars'] },
      },
      testMatch: '**/dropdown-width.spec.ts',
    },
  ],
  // The binary must exist: `cargo build --workspace` is part of the build
  // chain (see deploy/build.sh).
  webServer: {
    command: 'node e2e/serve.mjs',
    url: 'http://127.0.0.1:8099/api/status',
    reuseExistingServer: false,
    timeout: 60_000,
  },
  // Explicit shutdown of the disposable core, independent of the fate of the
  // webServer's node process: on Windows, Playwright ends that process with
  // `taskkill /T /F`, which only kills the Windows tree — not the Linux
  // process that `wsl.exe` launched inside the WSL2 VM (a lightweight VM,
  // outside that tree). See e2e/teardown.mjs for the details of the WSL-side
  // shutdown.
  globalTeardown: './e2e/teardown.mjs',
})

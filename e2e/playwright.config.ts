import { defineConfig, devices } from '@playwright/test';

const PORT = Number(process.env.SORTER2_E2E_PORT ?? 8090);
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './tests',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  reporter: [['list']],
  use: {
    baseURL: BASE_URL,
    trace: 'on-first-retry',
    video: process.env.PW_VIDEO === '1' ? 'on' : 'off',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: {
    // Build (no-op if already built) then run the release binary against a throwaway data dir.
    command:
      'cargo build --release --package sorter2-server && ' +
      `SORTER2_DATA_DIR="$(mktemp -d)" PORT=${PORT} ` +
      'target/release/sorter2-server',
    cwd: '..',
    url: `${BASE_URL}/healthz`,
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
  },
});

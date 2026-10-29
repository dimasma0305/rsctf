import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  testMatch: '*.spec.mjs',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 180_000,
  expect: { timeout: 10_000 },
  outputDir: '../visual-audit-output/interactions/results',
  reporter: [
    ['list'],
    ['html', { outputFolder: '../visual-audit-output/interactions/playwright', open: 'never' }],
    ['./e2e/inventory-reporter.mjs'],
  ],
  use: {
    browserName: 'chromium',
    locale: 'en-US',
    colorScheme: 'dark',
    serviceWorkers: 'block',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    launchOptions: process.env.CHROME_BIN ? { executablePath: process.env.CHROME_BIN } : {},
  },
  projects: [
    { name: 'desktop', use: { viewport: { width: 1440, height: 1100 } } },
    { name: 'compact', use: { viewport: { width: 320, height: 568 } } },
  ],
})

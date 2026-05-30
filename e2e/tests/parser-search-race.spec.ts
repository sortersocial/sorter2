import { test, expect, Route } from '@playwright/test';

/**
 * Reproduces the parser search-box response race.
 *
 * The search box posts every (debounced) keystroke to POST /ui and eval()s the
 * returned JS, which morphs `#parser-panel` (input value + `#parser-output`).
 * Responses are applied in arrival order with no ordering guard, so a slow
 * response for an earlier query can land *after* a newer one and clobber it.
 *
 * To force the race deterministically we intercept POST /ui and delay the
 * response based on the typed query: the earlier query ("r/rust") is delayed
 * far longer than the later query ("r/aww"). The later query therefore renders
 * first, then the stale earlier response overwrites it.
 *
 * Correct behavior: the panel reflects the *latest* query the user typed
 * ("r/aww"). Before the sequence-number fix this assertion fails because the
 * stale "r/rust" response wins the race.
 */

const SLOW_QUERY = 'r/rust';
const FAST_QUERY = 'r/aww';
const SLOW_DELAY_MS = 700;
const FAST_DELAY_MS = 50;

function queryFromBody(body: string): string {
  const params = new URLSearchParams(body);
  return params.get('query') ?? '';
}

async function delayUiResponses(route: Route) {
  const body = route.request().postData() ?? '';
  const query = queryFromBody(body);
  const delay = query === SLOW_QUERY ? SLOW_DELAY_MS : FAST_DELAY_MS;
  await new Promise((r) => setTimeout(r, delay));
  await route.continue();
}

test('latest search query wins even when an earlier response is slower', async ({ page }) => {
  await page.goto('/');

  const input = page.locator('#parser-input');
  const output = page.locator('#parser-output');
  await expect(input).toBeVisible();

  // Count finished /ui responses so we can wait until both land.
  let uiResponses = 0;
  page.on('response', (resp) => {
    if (resp.url().endsWith('/ui')) uiResponses += 1;
  });

  await page.route('**/ui', delayUiResponses);

  // Type the slow query first, wait past the 120ms debounce so its request fires.
  await input.fill(SLOW_QUERY);
  await page.waitForTimeout(250);

  // Type the fast query; its request will fire and resolve well before the slow one.
  await input.fill(FAST_QUERY);

  // Wait until both /ui responses have come back, plus a buffer for eval()/morph.
  await expect.poll(() => uiResponses, { timeout: 5_000 }).toBeGreaterThanOrEqual(2);
  await page.waitForTimeout(300);

  // The panel must reflect the last query the user typed, not the stale one.
  await expect(output).toContainText(FAST_QUERY);
  await expect(output).not.toContainText(SLOW_QUERY);
  await expect(input).toHaveValue(FAST_QUERY);
});

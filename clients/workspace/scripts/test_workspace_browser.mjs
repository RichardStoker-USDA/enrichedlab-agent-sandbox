// Browser regression for the research workspace against the synthetic loopback preview.
// Each viewport and theme starts from a reset fixture and walks what a scientist does:
// find every lab's projects on the lab index and filter them, switch labs, read where an analysis came from, read a
// project's report (where it stands, the agent's reading, findings, results, the notebook, checks, data used and
// downloads) and open a figure into its analysis, trace the figure to its code, see a failed file read and retry it,
// ask the agent for changes, edit the question, start and pause the agent, share access, download everything, follow
// recorded steps, set up a new project, change display settings, as an administrator inspect and release a private
// project, draw a figure from a result table and edit it into new versions, read the commons and mark a record
// baseline for every lab, and review the links the lab's papers propose. The detailed-view walk opens a project on its
// Overview, opens the same records from its explorer, follows a figure's thread to the agent decision that proposed it,
// finds a file by name and lists what needs attention; the long-run walk pages a project's analyses, lists every step,
// scrolls a few hundred files a window at a time and marks the records that arrive while it is open.
import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { createRequire } from "node:module";
import { mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";

const url = process.env.WORKSPACE_BROWSER_URL;
const playwrightPath = process.env.PLAYWRIGHT_NODE_PATH;
const executablePath = process.env.WORKSPACE_BROWSER_EXECUTABLE;
if (!url || !playwrightPath || !executablePath) throw new Error("workspace_browser_environment_required");
const target = new URL(url);
if (target.protocol !== "http:" || !["127.0.0.1", "[::1]"].includes(target.hostname)) throw new Error("workspace_browser_loopback_required");
// Refuse anything but the synthetic preview before any control is touched.
const probe = await fetch(new URL("/api/v1/me", target));
if (probe.headers.get("x-workspace-preview") !== "synthetic") throw new Error("workspace_browser_synthetic_preview_required");

const main = "50000000-0000-4000-8000-000000000001";
const blank = "50000000-0000-4000-8000-000000000002";
const foreign = "50000000-0000-4000-8000-000000000003";
const firstTask = "10000000-0000-4000-8000-000000000001";
const generatedTask = "10000000-0000-4000-8000-000000000004";
const standaloneTask = "10000000-0000-4000-8000-000000000003";
const longRunProject = "50000000-0000-4000-8000-000000000007";
// The long run's comparison step, whose package holds the season table and R's figure of it.
const seasonTask = "11000000-0000-4000-8000-000000000026";
const home = (hash) => new URL(`/#/${hash}`, target).href;

const { chromium } = createRequire(import.meta.url)(playwrightPath);
const browser = await chromium.launch({ executablePath, headless: true });
const captures = process.env.WORKSPACE_BROWSER_CAPTURE_DIR;
if (captures) await mkdir(captures, { recursive: true });

async function reset() {
  const response = await fetch(new URL("/api/v1/fixture/reset", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(response.status, 200, "the synthetic preview must reset between runs");
}
// UC Davis publishes Aggie Gold and nine tints; a darkened or faded gold is none of them. Every opaque gold the page
// resolves, on elements and their marks, and every gold the stylesheets define must be one of these.
const UC_DAVIS_GOLDS = ["#ffbf00", "#ffc519", "#ffcc33", "#ffd24c", "#ffd966", "#ffdf80", "#ffe599", "#ffecb2", "#fff2cc", "#fff9e5"];
async function palette(page, where) {
  // Colours at rest: a 120 ms border or fill change passes through blends no one sees as a colour of the page.
  await page.evaluate(() => Promise.all(document.getAnimations().filter((a) => a instanceof CSSTransition).map((a) => a.finished.catch(() => undefined))));
  const golds = await page.evaluate(() => {
    const found = new Set();
    const hex = (c) => "#" + c.map((v) => v.toString(16).padStart(2, "0")).join("");
    const golden = ([r, g, b]) => {
      const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
      if (d === 0) return false;
      let h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
      h = (h * 60 + 360) % 360;
      const l = (max + min) / 510;
      return h >= 30 && h <= 60 && d / 255 / (1 - Math.abs(2 * l - 1)) >= 0.4;
    };
    const take = (value) => {
      const m = value.match(/^rgba?\((\d+), (\d+), (\d+)(?:, ([\d.]+))?\)$/);
      if (!m || (m[4] !== undefined && Number(m[4]) < 1)) return;
      const c = [m[1], m[2], m[3]].map(Number);
      if (golden(c)) found.add(hex(c));
    };
    const properties = ["color", "backgroundColor", "borderTopColor", "borderRightColor", "borderBottomColor", "borderLeftColor", "outlineColor"];
    for (const element of document.querySelectorAll("*")) {
      for (const pseudo of [null, "::before", "::after"]) {
        const style = getComputedStyle(element, pseudo);
        if (pseudo && style.content === "none") continue;
        for (const property of properties) take(style[property]);
      }
    }
    for (const sheet of document.styleSheets) {
      for (const rule of sheet.cssRules) {
        for (const m of rule.cssText.matchAll(/#([0-9a-f]{6}|[0-9a-f]{3})\b/gi)) {
          const h = m[1].length === 3 ? m[1].split("").map((x) => x + x).join("") : m[1];
          const c = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
          if (golden(c)) found.add(hex(c));
        }
      }
    }
    return [...found];
  });
  assert.deepEqual(golds.filter((c) => !UC_DAVIS_GOLDS.includes(c)), [], `${where}: a gold UC Davis does not publish`);
  const faded = await page.evaluate(() => [...document.querySelectorAll("button:disabled")].filter((b) => !getComputedStyle(b).filter.includes("grayscale")).length);
  assert.equal(faded, 0, `${where}: a disabled control must turn neutral, not fade its fill`);
}
async function capture(page, name) {
  await palette(page, name);
  if (captures) await page.screenshot({ path: join(captures, `${name}.png`), fullPage: true, animations: "disabled" });
}
async function noSidewaysScroll(page, where) {
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, `${where}: the page must not scroll sideways`);
}
async function go(page, mobile, name) {
  // The sidebar holds the places; below the wide layout it opens as a drawer from the bar.
  if (mobile) await page.getByRole("button", { name: "Open the sidebar" }).click();
  await page.getByRole("navigation", { name: "Places" }).getByRole("link", { name }).first().click();
}
// A project's own row in its lab's list on the home page; the waiting list above it may name the same project.
const projectRow = (page, title) => page.getByRole("main").locator("section.lab").getByRole("link", { name: new RegExp(`^${title}`) });
// WCAG contrast of an element's text against its own resolved background.
async function contrast(locator) {
  return locator.evaluate((element) => {
    const rgb = (value) => value.match(/[\d.]+/g).slice(0, 3).map(Number);
    let node = element, background = "rgba(0, 0, 0, 0)";
    while (node && /rgba\(0, 0, 0, 0\)|transparent/.test(background)) { background = getComputedStyle(node).backgroundColor; node = node.parentElement; }
    const luminance = (channels) => { const [r, g, b] = channels.map((c) => { const v = c / 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
    const [a, b] = [luminance(rgb(getComputedStyle(element).color)), luminance(rgb(background))].sort((x, y) => y - x);
    return (a + 0.05) / (b + 0.05);
  });
}

// A project is one page whose steps open in place; an analysis shows one section at a time under a tab bar.
function projectStep(page, title) {
  return page.locator("#section-steps li.story-step").filter({ has: page.locator(".story-title", { hasText: title }) }).first();
}
async function openStep(page, title) {
  const step = projectStep(page, title);
  if (!(await step.locator("details").evaluate((d) => d.open))) await step.locator("summary").first().click();
  return step;
}
async function analysisTab(page, label) {
  const tab = page.getByRole("tablist", { name: "Sections of this analysis" }).getByRole("tab", { name: new RegExp(`^${label}`) });
  await tab.click();
  assert.equal(await tab.getAttribute("aria-selected"), "true", `the ${label} tab opens`);
}

// A hidden panel has no layout to check, so each tab's panel is checked for sideways scroll while it shows.
async function everyTabFits(page, tablist, where) {
  const tabs = page.getByRole("tablist", { name: tablist }).getByRole("tab");
  const count = await tabs.count();
  assert.ok(count > 1, `${where}: the tabs are there`);
  for (let i = 0; i < count; i++) {
    await tabs.nth(i).click();
    assert.equal(await tabs.nth(i).getAttribute("aria-selected"), "true");
    await noSidewaysScroll(page, `${where}, tab ${i + 1}`);
  }
  await tabs.first().click();
}

// Shows one of the Detailed view's side views: from the activity bar, or on a phone from the Explorer pane's switch.
// A tool already showing is left as it is, since pressing it again closes the side pane.
async function sideView(page, mobile, label) {
  if (mobile) await page.getByRole("navigation", { name: "Detailed view panes" }).getByRole("button", { name: "Explorer", exact: true }).click();
  const button = mobile ? page.getByRole("group", { name: "Side pane" }).getByRole("button", { name: new RegExp(`^${label}`) })
    : page.getByRole("navigation", { name: "Detailed view tools" }).getByRole("button", { name: new RegExp(`^${label}`) });
  if (await button.getAttribute("aria-pressed") !== "true") await button.click();
}

async function workbench(page, mobile, name) {
  // The start page lists the reader's recent projects with where each stands, and one opens from it.
  await page.goto(home("workbench"), { waitUntil: "domcontentloaded" });
  if (mobile) await page.getByRole("navigation", { name: "Detailed view panes" }).getByRole("button", { name: "Open record", exact: true }).click();
  const recent = page.getByRole("region", { name: "Recent projects" });
  const listed = recent.getByRole("button", { name: /^Test diet response study/ });
  await listed.waitFor();
  assert.match(await listed.innerText(), /Waiting for you|Working now|Paused|Finished|Did not finish|Not started/,
    `${name}: a recent project says where it stands`);
  assert.ok(await recent.getByRole("button").count() <= 8, `${name}: the start page lists at most eight projects`);
  // The start page follows the lab chosen at the top, as every page does: another lab's projects stay out until it is
  // chosen, and the status bar counts the chosen lab's projects.
  await recent.getByRole("heading", { name: "Recent projects in Example Nutrition Lab" }).waitFor();
  assert.equal(await recent.getByRole("button", { name: /Field sampling plan/ }).count(), 0, `${name}: another lab's project stays off the start page`);
  assert.match(await page.locator("footer.status").textContent(), /\d+ projects? in Example Nutrition Lab/, `${name}: the status bar counts the chosen lab's projects`);
  const masthead = page.locator("header.bar");
  {
    await masthead.getByRole("button", { name: /^Change lab/ }).click();
    await page.locator("#lab-menu").getByRole("button", { name: /Example Field Lab/ }).click();
    await recent.getByRole("heading", { name: "Recent projects in Example Field Lab" }).waitFor();
    await recent.getByRole("button", { name: /^Field sampling plan/ }).waitFor();
    assert.equal(await recent.getByRole("button", { name: /Test diet response study/ }).count(), 0, `${name}: the first lab's projects leave with it`);
    await masthead.getByRole("button", { name: /^Change lab/ }).click();
    await page.locator("#lab-menu").getByRole("button", { name: /Example Nutrition Lab/ }).click();
    await recent.getByRole("heading", { name: "Recent projects in Example Nutrition Lab" }).waitFor();
    // A project still loading when another lab is chosen does not reopen when its read arrives.
    let release = () => {};
    const held = new Promise((resolve) => { release = resolve; });
    const projectRead = `**/api/v1/projects/${main}`;
    await page.route(projectRead, async (route) => { await held; await route.continue(); }, { times: 1 });
    await recent.getByRole("button", { name: /^Test diet response study/ }).click();
    await masthead.getByRole("button", { name: /^Change lab/ }).click();
    await page.locator("#lab-menu").getByRole("button", { name: /Example Field Lab/ }).click();
    await recent.getByRole("heading", { name: "Recent projects in Example Field Lab" }).waitFor();
    const answered = page.waitForResponse((response) => new URL(response.url()).pathname === `/api/v1/projects/${main}`);
    release();
    await answered;
    await page.waitForTimeout(1500);
    assert.equal(await page.getByRole("tab", { name: "Overview" }).count(), 0, `${name}: a project that finished loading after the lab changed stays closed`);
    assert.equal(new URL(page.url()).hash, "#/workbench", `${name}: the address stays on the chosen lab's start page`);
    await recent.getByRole("heading", { name: "Recent projects in Example Field Lab" }).waitFor();
    await masthead.getByRole("button", { name: /^Change lab/ }).click();
    await page.locator("#lab-menu").getByRole("button", { name: /Example Nutrition Lab/ }).click();
    await recent.getByRole("heading", { name: "Recent projects in Example Nutrition Lab" }).waitFor();
  }
  for (const key of ["Ctrl", "Enter", "The thread"]) await page.getByRole("region", { name: "Keys" }).getByText(key, { exact: true }).first().waitFor();
  await noSidewaysScroll(page, `workbench start ${name}`);
  await capture(page, `workbench_start_${name}`);
  // Opened from the keyboard, the project's Overview tab takes the focus the vanished button held.
  await listed.focus();
  await page.keyboard.press("Enter");
  const opened = page.getByRole("tab", { name: "Overview" });
  await opened.waitFor();
  assert.ok(await opened.evaluate((tab) => tab === document.activeElement), `${name}: focus moves to the opened project's Overview`);
  assert.match(new URL(page.url()).hash, /^#\/workbench\/projects\/[0-9a-f-]{36}$/, `${name}: a recent project opens at its address`);
  // Choosing another lab at the top keeps the Detailed view open, closes the records left open and starts on that
  // lab's projects.
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Field Lab/ }).click();
  await recent.getByRole("heading", { name: "Recent projects in Example Field Lab" }).waitFor();
  // The address follows once the page's transition ends.
  await page.waitForFunction(() => location.hash === "#/workbench", null, { timeout: 5000 }).catch(() => {});
  assert.equal(new URL(page.url()).hash, "#/workbench", `${name}: the Detailed view stays open on the chosen lab`);
  assert.equal(await page.getByRole("tab", { name: "Overview" }).count(), 0, `${name}: the other lab's records close`);
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Nutrition Lab/ }).click();
  await recent.getByRole("heading", { name: "Recent projects in Example Nutrition Lab" }).waitFor();
  await page.goto(home(`workbench/projects/${main}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("tab", { name: "Overview" }).waitFor();
  assert.match(new URL(page.url()).hash, /^#\/workbench\/projects\/[0-9a-f-]{36}$/, "the workbench address carries only the project identifier");
  const pane = (label) => mobile ? page.getByRole("navigation", { name: "Detailed view panes" }).getByRole("button", { name: label, exact: true }).click() : Promise.resolve();
  // The Overview: where the project stands in the project page's words, and the agent's answer, its values read from
  // the result tables they name. A value opens the table it came from.
  const overview = page.locator(".view[data-kind='question']");
  await overview.getByRole("heading", { name: "Test diet response study", level: 1 }).waitFor();
  await overview.getByText("The agent stopped and is waiting for your review.", { exact: true }).waitFor();
  await overview.getByText("The agent's reading, not checked by a person.", { exact: true }).waitFor();
  const value = overview.locator("data.bound").first();
  await value.waitFor();
  assert.equal(await value.textContent(), "12.38", `${name}: the first finding's value is read from its table`);
  // The answer's table shows here too, its rows from the digest, with a link to the table inside its package.
  const overviewTable = overview.getByRole("region", { name: "Every pairwise comparison between the diets, scroll for more" });
  await overviewTable.waitFor();
  assert.equal(await overviewTable.locator("tbody tr").count(), 3, `${name}: the Overview's answer table has every row`);
  assert.equal(await overview.getByRole("button", { name: "Open the table" }).count(), 1, `${name}: the answer table opens its file`);
  assert.equal(await overview.getByText("A person asked for changes to an analysis this table reads", { exact: true }).count(), 1,
    `${name}: the Overview marks the answer table on the reviewed analysis`);
  await overview.getByRole("list", { name: "The agent's steps" }).getByRole("button", { name: "Step 2: Compare plasma response across the three diets" }).waitFor();
  await noSidewaysScroll(page, `workbench overview ${name}`);
  await capture(page, `workbench_overview_${name}`);
  await value.click();
  await page.getByRole("tab", { name: "Group summary" }).waitFor();
  await page.getByRole("navigation", { name: "Where this record sits" }).getByText("Step 2", { exact: true }).waitFor();
  await pane("Explorer");
  // The explorer shows the open project under its picker: the question, its steps in plan order, analyses outside the
  // plan and the record, each group its own tree with one Tab stop.
  const tree = page.getByRole("complementary", { name: "Explorer" });
  await tree.getByRole("button", { name: /^Project: Test diet response study/ }).waitFor();
  for (const group of ["The question and the answer", "Steps", "Also in this project", "The record"]) await tree.getByRole("tree", { name: group }).waitFor();
  const steps = tree.getByRole("tree", { name: "Steps" });
  await steps.getByRole("treeitem", { name: /^1 · Summarize each diet and test feature changes, Finished/ }).waitFor();
  await tree.getByRole("tree", { name: "Also in this project" }).getByRole("treeitem", { name: /^Evidence review, Did not finish/ }).waitFor();
  assert.deepEqual(await tree.getByRole("tree").evaluateAll((trees) => trees.map((t) => t.querySelectorAll('[role="treeitem"][tabindex="0"]').length)), [1, 1, 1, 1],
    `${name}: each tree of the explorer has one Tab stop`);
  const row = (label) => tree.getByRole("treeitem", { name: label }).locator(":scope > .row");
  // The filter finds records in the open project, and a result opens from the keyboard.
  await tree.getByRole("searchbox", { name: "Filter this project" }).fill("volcano");
  const match = tree.getByRole("list", { name: "Records that match" }).getByRole("button", { name: /^Feature volcano/ }).first();
  await match.focus();
  await page.keyboard.press("Enter");
  await page.getByRole("tab", { name: "Feature volcano" }).waitFor();
  await pane("Explorer");
  // Assistive technology activates the item element itself, not the row inside it.
  await tree.getByRole("treeitem", { name: /^Work sessions/ }).evaluate((element) => element.click());
  assert.equal(await tree.getByRole("treeitem", { name: /^Work sessions/ }).getAttribute("aria-expanded"), "true",
    `${name}: activating a tree item element must open it`);
  // The explorer moves like a code editor's: open the step, then its figures, from the keyboard.
  await row(/^2 · Compare plasma response across the three diets/).click();
  if (mobile) await pane("Explorer");
  const compare = tree.getByRole("treeitem", { name: /^2 · Compare plasma response across the three diets/ });
  await compare.focus();
  // The table opened from the Overview left this step open; the left arrow closes it first.
  if (await compare.getAttribute("aria-expanded") === "true") await page.keyboard.press("ArrowLeft");
  for (const key of ["ArrowRight", "ArrowDown", "ArrowRight", "ArrowDown", "Enter"]) await page.keyboard.press(key);
  await page.getByRole("tab", { name: "Response by diet" }).waitFor();
  // The open record keeps its tree's Tab stop, found through its ancestry, not the tree's first row; a figure R saved
  // twice names both formats.
  if (mobile) await pane("Explorer");
  const stop = await steps.locator('[role="treeitem"][tabindex="0"]').evaluateAll((items) => items.map((i) => i.getAttribute("aria-label")));
  assert.equal(stop.length, 1, `${name}: one Tab stop in the steps`);
  assert.match(stop[0], /^Response by diet, response_by_diet\.svg, PDF, SVG/, `${name}: the open file holds the Tab stop of its tree`);
  // The inspector shows the open record's details and its thread back to the question; on a phone it is a pane.
  if (mobile) await pane("Thread");
  const inspector = page.getByRole("complementary", { name: "Inspector" });
  await inspector.getByText("Matches the package checksum list", { exact: true }).waitFor();
  const trace = page.getByRole("list", { name: "How this record connects to the question" });
  for (const role of ["Made by", "From inputs", "Analysis", "Proposed in", "Question"]) await trace.getByText(role, { exact: true }).waitFor();
  await capture(page, `workbench_trace_${name}`);
  await trace.getByRole("button", { name: /^Decision 2:/ }).click();
  await page.getByRole("heading", { name: "Decision 2", level: 1 }).waitFor();
  await page.getByText("The agent's reading of the project at this decision. It is not a verified result.").waitFor();
  await page.keyboard.press("Control+K");
  const find = page.getByRole("combobox");
  await find.fill("diet comparisons");
  await page.getByRole("option", { name: /Diet comparisons/ }).first().waitFor();
  await find.press("Enter");
  await page.getByRole("tab", { name: "Diet comparisons" }).waitFor();
  // Needs attention groups what went wrong by kind; a stop for review is where the project stands, not a problem.
  await sideView(page, mobile, "Needs attention, 2");
  const attention = page.getByRole("complementary", { name: "Needs attention" });
  for (const group of ["Did not finish 1", "Replies the system refused 1"]) await attention.getByRole("heading", { name: group }).waitFor();
  await attention.getByText("This happened 2 times; no analysis ran because of it.", { exact: false }).waitFor();
  assert.equal(await attention.getByText(/waiting for your review/).count(), 0, `${name}: a review stop is not a problem`);
  // The timeline opens from the activity bar, or as a pane on a phone: every saved version, decision and access change.
  if (mobile) await pane("Timeline");
  else await page.getByRole("navigation", { name: "Detailed view tools" }).getByRole("button", { name: "Timeline", exact: true }).click();
  await page.locator(".panel-body").getByText("Administrator inspection started").first().waitFor();
  const status = page.locator('footer[aria-label="Status"] span').first();
  assert.ok(await contrast(status) >= 4.5, `${name}: workbench status text must meet WCAG AA`);
  await noSidewaysScroll(page, `workbench ${name}`);
  await capture(page, `workbench_${name}`);
}

// A long run (Glass Box, "Long runs"): thirty planned steps, more analyses than one project read returns, and a few
// hundred files. The read follows the analyses cursor; the explorer lists every step in plan order with what is new;
// All files draws only the rows in view; records that arrive while the page is open are marked; the status line says
// what holds the run back from records; Watch it work follows the record as it is written; and the project page never
// folds a step that failed or needs a check into "finished".
async function longRun(page, mobile, name) {
  const pane = (label) => mobile ? page.getByRole("navigation", { name: "Detailed view panes" }).getByRole("button", { name: label, exact: true }).click() : Promise.resolve();
  const pages = [];
  const listen = (response) => { const at = new URL(response.url()); if (at.pathname === `/api/v1/projects/${longRunProject}/analyses`) pages.push(at.search); };
  page.on("response", listen);
  await page.goto(home(`workbench/projects/${longRunProject}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("tab", { name: "Overview" }).waitFor();
  await page.getByRole("heading", { name: "Canopy temperature across the season", level: 1 }).waitFor();
  page.off("response", listen);
  assert.deepEqual(pages.slice(0, 2), ["?cursor=10&limit=200", "?cursor=20&limit=200"], `${name}: the read follows the analyses cursor to its end`);
  // The letterhead follows the lab of the project being read.
  await page.locator("header.bar").getByRole("button", { name: /^Change lab\. You are in Example Field Lab\./ }).waitFor();
  await sideView(page, mobile, "Explorer");
  const tree = page.getByRole("complementary", { name: "Explorer" });
  const steps = tree.getByRole("tree", { name: "Steps" });
  await steps.getByRole("treeitem", { name: /^1 · Check the logger records for gaps/ }).waitFor();
  assert.equal(await steps.locator(':scope > [role="treeitem"]').count(), 30, `${name}: every planned step is a row`);
  for (const label of [/^16 · Summarize block 15, Needs a check/, /^20 · Summarize block 19, Did not finish/, /^27 · Fit the afternoon temperature model, Running/,
    /^28 · Check the model's residuals, Planned/]) await steps.getByRole("treeitem", { name: label }).waitFor();
  // Nine steps ended since the reader's last look; block 19's row shows its failure mark and still says it is new.
  assert.equal(await steps.getByRole("treeitem", { name: /new since your last look$/ }).count(), 9, `${name}: the steps that ended since the last look are new`);
  // All files: every row says its place in the whole list, and only the rows in view are drawn.
  await tree.getByRole("treeitem", { name: /^All files, 274/ }).click();
  const files = page.getByRole("listbox", { name: "Files, by step" });
  await files.waitFor();
  const drawn = await files.getByRole("option").count();
  assert.ok(drawn > 0 && drawn < 60, `${name}: only the rows in view are drawn (${drawn} of 274)`);
  assert.equal(await files.getByRole("option").first().getAttribute("aria-setsize"), "274", `${name}: a row knows the size of the whole list`);
  await files.evaluate((list) => { list.scrollTop = list.scrollHeight; list.dispatchEvent(new Event("scroll")); });
  await files.getByRole("option", { name: /^Run parameters, Step 26: Compare the blocks/ }).waitFor();
  const kinds = page.getByRole("group", { name: "Show files of one kind" });
  await kinds.getByRole("button", { name: /^New since your last look/ }).click();
  await page.getByText("105 of 274 files", { exact: true }).waitFor();
  await kinds.getByRole("button", { name: /^New since your last look/ }).click();
  await kinds.getByRole("button", { name: /^Tables/ }).click();
  await page.getByText("50 of 274 files", { exact: true }).waitFor();
  await page.getByRole("searchbox", { name: "Find a file by name" }).fill("block 17 hourly");
  await page.getByText("1 of 274 files", { exact: true }).waitFor();
  await files.focus();
  await page.keyboard.press("Enter");
  await page.getByRole("tab", { name: "Block 17 hourly" }).waitFor();
  await noSidewaysScroll(page, `long run files ${name}`);
  await capture(page, `long_run_files_${name}`);
  // Needs attention counts the failed step, the step to check and the refused reply.
  await sideView(page, mobile, "Needs attention, 3");
  const attention = page.getByRole("complementary", { name: "Needs attention" });
  for (const group of ["Did not finish 1", "Steps to check 1", "Replies the system refused 1"]) await attention.getByRole("heading", { name: group }).waitFor();
  // The run moves on while the page is open: the model finishes and the residual check waits for a compute slot. The
  // explorer marks what arrived and the status bar says what the system does now.
  await sideView(page, mobile, "Explorer");
  const advance = async () => assert.equal((await fetch(new URL("/api/v1/fixture/long-run-advances", target),
    { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } })).status, 200);
  await advance();
  await steps.getByRole("treeitem", { name: /^27 · Fit the afternoon temperature model, Finished/ }).waitFor({ timeout: 15000 });
  await steps.getByRole("treeitem", { name: /^28 · Check the model's residuals, Waiting to start/ }).waitFor();
  await page.locator('footer[aria-label="Status"] [aria-live="polite"]').filter({ hasText: /^New in the explorer: / }).waitFor({ state: "attached" });
  if (!mobile) await page.locator('footer[aria-label="Status"]').getByText("Waiting for a free compute slot · under a minute on this step", { exact: true }).waitFor();
  await noSidewaysScroll(page, `long run ${name}`);
  await capture(page, `long_run_${name}`);
  // Slow is a state, not a failure (long runs rule 2): the project page names the wait for a compute slot, where the
  // analysis stands in line as the broker records it, and the recent analyses' waits and run times from their recorded
  // times.
  await page.goto(home(`projects/${longRunProject}`), { waitUntil: "domcontentloaded" });
  const summary = page.locator("#section-summary");
  await summary.getByText("Waiting for a free compute slot", { exact: true }).waitFor();
  await summary.getByText("1 analysis submitted earlier is ahead of it. The lab's compute slots are all in use.", { exact: true }).waitFor();
  await summary.getByText("The last 5 analyses waited 16 min for a compute slot and ran 13 min, at the median. The 5 before them waited 2 min and ran 12 min.",
    { exact: true }).waitFor();
  // Watching is optional (rule 3): Watch it work opens the record as it is written, newest first, and follows it.
  const watchIt = summary.getByRole("button", { name: "Watch it work", exact: true });
  assert.equal(await watchIt.getAttribute("aria-expanded"), "false", `${name}: the record starts closed`);
  await watchIt.click();
  const watching = page.getByRole("region", { name: "Watching it work" });
  const feed = watching.getByRole("list", { name: "The record, newest first" });
  await feed.getByRole("listitem").first().filter({ hasText: "Waiting for a free compute slot, under a minute on this step" }).waitFor();
  assert.equal(await page.evaluate(() => document.activeElement?.id), "watch-title", `${name}: opening it moves the keyboard to its heading`);
  const texts = (await feed.getByRole("listitem").allTextContents()).map((t) => t.replace(/\s+/g, " ").trim());
  const order = ["Step 28 requested: Check the model's residuals", "Methods check agreed: Residuals by block", "Decision 7: The model finished.",
    "Step 27 finished: 2 tables, 1 code file, 2 logs and 5 other files, each matching its checksum list"].map((part) => texts.findIndex((t) => t.includes(part)));
  assert.ok(order.every((at, i) => at > 0 && (i === 0 || at > order[i - 1])), `${name}: the record reads newest first (${order.join(", ")})`);
  // A compute slot takes the residual check while the record is open: its recorded steps arrive and are read out once.
  await advance();
  await feed.getByRole("listitem").filter({ hasText: "Step 28: Analysis started" }).waitFor({ timeout: 15000 });
  await feed.getByRole("listitem").first().filter({ hasText: "Running the analysis, under a minute on this step" }).waitFor();
  await watching.locator('[aria-live="polite"]').filter({ hasText: /^New in the record: Step 28: / }).waitFor({ state: "attached" });
  await noSidewaysScroll(page, `long run watching ${name}`);
  await capture(page, `long_run_watch_${name}`);
  await watching.getByRole("button", { name: "Close", exact: true }).click();
  await watching.waitFor({ state: "detached" });
  assert.equal(await watchIt.evaluate((el) => el === document.activeElement), true, `${name}: closing it returns the keyboard to Watch it work`);
  // The project page reads the same records: one bar with counts for a long plan, and a fold that names every state.
  for (const words of ["25 finished", "1 working", "1 to check", "1 did not finish", "2 planned"]) await summary.getByText(words, { exact: true }).waitFor();
  await page.locator("#section-steps .story-fold summary .story-detail").filter({ hasText: /^22 finished, 1 to check, 1 did not finish/ }).waitFor();
  await noSidewaysScroll(page, `long run page ${name}`);
  await capture(page, `long_run_page_${name}`);
  // The lab choice is kept in the browser; the walks after this one start in the first lab.
  const masthead = page.locator("header.bar");
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Nutrition Lab/ }).click();
  await masthead.getByRole("button", { name: /^Change lab\. You are in Example Nutrition Lab\./ }).waitFor();
}

// A running step's output as it is written (Glass Box results rule 6). The log starts closed;
// opened, it follows new lines until the reader scrolls up, then offers a jump back with the count of new lines. A
// progress count rewrites its own line; the step's lifecycle (System) filters apart; find, times, wrap and copy. The text
// is not yet checked while it runs; a stream that ends is checked against the broker's record, and one whose text does
// not match says so.
async function liveLog(page, mobile, name) {
  const residual = "11000000-0000-4000-8000-000000000028";
  // The long-run walk gave the residual check its compute slot; its output starts again from the first line.
  assert.equal((await fetch(new URL("/api/v1/fixture/long-run-log-restarts", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } })).status, 200);
  await page.goto(home(`projects/${longRunProject}/analyses/${residual}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Check the model's residuals", level: 1 }).waitFor();
  assert.equal(await page.getByRole("region", { name: "Output", exact: true }).count(), 0, `${name}: the log starts closed`);
  await page.getByRole("button", { name: "Watch the log", exact: true }).click();
  const log = page.getByRole("region", { name: "Output", exact: true });
  await log.getByText(/^Live\. Not yet checked;/).waitFor();
  const body = log.getByRole("log", { name: "Output of this step" });
  await body.getByText("Rscript code/check_residuals.R", { exact: true }).waitFor();
  // A progress count rewrites its own line, so only the finished count is left.
  await body.getByText("Residuals by block 100%", { exact: true }).waitFor({ timeout: 15000 });
  assert.equal(await body.getByText(/^Residuals by block +\d+%$/).count(), 1, `${name}: a progress count is one line`);
  // Colour and weight codes read as tones; nothing else from a terminal reaches the page.
  await body.locator(".tone-strong", { hasText: "Warning:" }).waitFor();
  // Scrolled up while lines arrive, the log stops following and counts what came since.
  await body.getByText(/^block\s+block_days$/).waitFor({ timeout: 15000 });
  await body.evaluate((el) => { el.scrollTop = 0; el.dispatchEvent(new Event("scroll")); });
  const jump = log.getByRole("button", { name: /^\d+ new lines? · Jump to the newest$/ });
  await jump.waitFor({ timeout: 10000 });
  await jump.click();
  await jump.waitFor({ state: "detached" });
  assert.ok(await body.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight < 24), `${name}: the jump returns to the newest line`);
  await body.getByText(/^Drawing the residual plots/).waitFor({ timeout: 10000 });
  // Find moves between matches inside the log; Times and Wrap change only how the text is shown.
  const find = log.getByRole("searchbox", { name: "Find in the output" });
  await find.fill("residuals");
  await log.getByText(/^1 of \d+$/).waitFor();
  await find.press("Enter");
  await log.getByText(/^2 of \d+$/).waitFor();
  assert.equal(await body.locator("mark.current").count(), 1, `${name}: one match is the current one`);
  await find.fill("");
  await log.getByRole("checkbox", { name: "Times" }).check();
  assert.ok(await body.locator("time").count() > 10, `${name}: each line shows its time`);
  const long = body.locator(".row", { hasText: "Drawing the residual plots" });
  const wrapped = (await long.boundingBox()).height;
  await log.getByRole("checkbox", { name: "Wrap lines" }).uncheck();
  assert.ok((await long.boundingBox()).height < wrapped, `${name}: without wrap a long line stays one line`);
  await log.getByRole("checkbox", { name: "Wrap lines" }).check();
  // The step's own lifecycle reads apart under System.
  const show = log.getByRole("radiogroup", { name: "Show" });
  assert.deepEqual(await show.getByRole("radio").evaluateAll((radios) => radios.map((r) => r.parentElement.textContent.trim())),
    ["All", "Messages and errors", "System"], `${name}: the cluster's merged stream is one filter`);
  await show.getByText("System", { exact: true }).click();
  await body.getByText("Assigned to a compute worker", { exact: true }).waitFor();
  assert.equal(await body.getByText("Rscript code/check_residuals.R").count(), 0, `${name}: System shows only the step's lifecycle`);
  await show.getByText("All", { exact: true }).click();
  // Copy takes the text shown as plain text.
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: target.origin });
  await log.getByRole("button", { name: "Copy", exact: true }).click();
  await log.getByText(/^Copied \d+ lines\.$/).waitFor();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  assert.ok(copied.includes("Residuals by block 100%") && !copied.includes("\x1b"), `${name}: the copy is plain text`);
  // A grant rejects every permission it does not list, so the walks after this one get the browser's defaults back.
  await page.context().clearPermissions();
  await noSidewaysScroll(page, `live log ${name}`);
  await capture(page, `live_log_${name}`);
  // A stream that ends is checked against the broker's record of the last attempt; a text that differs says so.
  const texts = ["Rscript code/check_residuals.R\n", "\x1b[31mError\x1b[39m in fit: block 19 has no rows\n", "Wrote results/residuals_by_block.csv\n"];
  const record = texts.join("");
  const saved = randomUUID();
  const stream = (closed) => [
    { seq: 1, attempt: 1, text: "first attempt\n" }, ...texts.map((text, i) => ({ seq: i + 2, attempt: 2, text, dropped: i === 1 ? 12 : 0 })),
  ].map((p) => `id: ${p.seq}\nevent: piece\ndata: ${JSON.stringify({ stream: "combined", bytes: Buffer.byteLength(p.text), at: new Date().toISOString(), dropped: 0, ...p })}\n\n`).join("")
    + `event: closed\ndata: ${JSON.stringify(closed)}\n\n`;
  const recorded = (sha256) => ({ attempt: 2, recorded: true, pieces: 3, size: Buffer.byteLength(record), sha256, dropped: 0,
    artifacts: [{ artifact: saved, role: "stdout", sha256: "0".repeat(64), size: 2048 }] });
  const route = `**/api/v1/tasks/${residual}/log/stream*`;
  for (const [closed, words] of [[recorded(createHash("sha256").update(record).digest("hex")), /^Ended\. The text shown is the text the broker recorded: 3 pieces, /],
    [recorded("0".repeat(64)), /^Ended, but the text shown here does not match the broker's record\./],
    // A step that ended without a closure record: its lines were never checked, and nothing was saved.
    [{ attempt: 2, recorded: false, reason: "no_closure", pieces: null, size: null, sha256: null, dropped: null, artifacts: [] },
      /^Ended\. The broker kept no record of this text, so it cannot be checked\.$/]]) {
    await page.route(route, (r) => r.fulfill({ status: 200, contentType: "text/event-stream", body: stream(closed) }));
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Watch the log", exact: true }).click();
    const ended = page.getByRole("region", { name: "Output", exact: true });
    await ended.getByText(words).waitFor();
    await ended.getByRole("log").getByText("Attempt 2 started", { exact: true }).waitFor();
    await ended.locator(".tone-alarm", { hasText: "Error" }).waitFor();
    await ended.getByText("12 bytes of terminal control codes were removed before the text reached the page.", { exact: true }).waitFor();
    if (closed.artifacts.length) await ended.getByRole("button", { name: "messages (2.0 KB)" }).waitFor();
    await page.unroute(route);
  }
  await capture(page, `live_log_ended_${name}`);
  // The Detailed view's Output panel shows the same log for the running step it has open. The project opens first, so
  // a phone's pane is chosen after the project has taken it.
  await page.goto(home(`workbench/projects/${longRunProject}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("tab", { name: "Overview" }).waitFor();
  await page.getByRole("heading", { name: "Canopy temperature across the season", level: 1 }).waitFor();
  await sideView(page, mobile, "Explorer");
  await page.getByRole("complementary", { name: "Explorer" }).getByRole("tree", { name: "Steps" })
    .getByRole("treeitem", { name: /^28 · Check the model's residuals, Running/ }).click();
  if (mobile) await page.getByRole("navigation", { name: "Detailed view panes" }).getByRole("button", { name: "Timeline", exact: true }).click();
  await page.getByRole("tablist", { name: "Panel" }).getByRole("tab", { name: "Output", exact: true }).click();
  const panel = page.getByRole("region", { name: "Output", exact: true });
  await panel.getByRole("log", { name: "Output of this step" }).getByText("Rscript code/check_residuals.R", { exact: true }).waitFor();
  await panel.getByText(/^Live\. Not yet checked;/).waitFor();
  await noSidewaysScroll(page, `detailed live log ${name}`);
  await capture(page, `live_log_detailed_${name}`);
  // The lab choice is kept in the browser; the walks after this one start in the first lab.
  const masthead = page.locator("header.bar");
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Nutrition Lab/ }).click();
  await masthead.getByRole("button", { name: /^Change lab\. You are in Example Nutrition Lab\./ }).waitFor();
}

// Results of every kind (Glass Box, results rules 1 to 3). Files that hold the same numbers are one
// result whose views are one switch apart: 3D, R figure and values. Values come a page at a time from the stored file,
// sorted by the server by type; a pick in one view stays picked in the others and names its row and line; the 3D view
// prints its scale and draws on the GPU or shows R's figure in its place. Space opens Quick Look from the file list, the
// arrow keys move to the next file, keys inside a view stay with it, and Escape returns to the file the reader ended on.
async function resultFrames(page, mobile, name) {
  await page.goto(home(`analyses/${seasonTask}`), { waitUntil: "domcontentloaded" });
  await analysisTab(page, "Files");
  const list = page.getByRole("list", { name: "Retained files" });
  const table = list.getByRole("button", { name: /^Season difference A results table/ });
  await table.click();
  const reader = page.locator(".file-reader");
  const views = reader.getByRole("tablist", { name: "Views of Season difference" });
  assert.deepEqual(await views.getByRole("tab").allInnerTexts(), ["3D", "R figure", "Values"], `${name}: one result, three views`);
  assert.equal(await views.getByRole("tab", { name: "Values" }).getAttribute("aria-selected"), "true", `${name}: a table opens on its values`);
  // The file record says how the broker checked the file against its kind.
  const record = reader.locator("details.technical");
  await record.locator("summary").click();
  await record.getByText("Passed as table.delimited version 1.", { exact: true }).waitFor();
  await record.locator("summary").click();
  const values = reader.getByRole("region", { name: "Season difference, values" });
  await values.getByText("552 rows, 3 columns. Pages come from the stored file; its checksum matches.", { exact: true }).waitFor();
  await values.getByText("Rows 1 to 100 of 552", { exact: true }).waitFor();
  // The server sorts by the column's type: the largest difference first, 4.142 at block 11, hour 13.
  const header = values.getByRole("columnheader", { name: "difference_c" });
  await header.getByRole("button").click();
  await header.getByRole("button").click();
  assert.equal(await header.getAttribute("aria-sort"), "descending", `${name}: the column says how it is sorted`);
  const top = values.locator("tbody tr").first();
  await top.filter({ hasText: "4.142" }).waitFor();
  await top.getByRole("button", { name: "Pick row 254, line 255", exact: true }).click();
  const caption = reader.getByRole("status").filter({ hasText: /^Picked: / });
  assert.equal((await caption.innerText()).replace(/\s+/g, " ").trim(),
    "Picked: block 11, hour 13, difference_c 4.142 · row 254, line 255 of results/season_difference.csv Clear", `${name}: a pick names its values, row and line`);
  // Column summaries come from the server's routine with a receipt the page checks before showing them.
  await values.getByRole("button", { name: "Columns", exact: true }).click();
  await values.getByText(/^Column summaries by enrichedlab\.table-summary version 1, read from the stored file; types are inferred/).waitFor();
  assert.equal((await values.locator(".summary tbody tr").nth(2).innerText()).replace(/\s+/g, " ").trim(), "difference_c number 552 0 -1.592 4.142 499",
    `${name}: each column's type, count, missing, range and distinct values`);
  // The 3D view: the stored points as a surface on the GPU, or R's figure of the same numbers when no GPU path works.
  await views.getByRole("tab", { name: "3D" }).click();
  const surface = reader.getByRole("region", { name: "Season difference, 3D view" });
  await surface.getByText(/^Vertical scale ×1 · drawn with (WebGPU|WebGL 2)$|R's figure of the same numbers is shown/).waitFor({ timeout: 20000 });
  const drawn = await surface.getByRole("application").count() > 0;
  await surface.getByText("Across: hour, 0 to 23. Deep: block, 1 to 24. 552 stored points, each drawn once. No rows for block 19, so the surface has a gap there.", { exact: true }).waitFor();
  if (drawn) {
    const view = surface.getByRole("application", { name: /^3D view of difference_c by hour and block\. Arrow keys turn it/ });
    await view.focus();
    for (const key of ["ArrowLeft", "Shift+ArrowUp", "+", "0"]) await page.keyboard.press(key);
    await surface.getByRole("radio", { name: "×2" }).check();
    await surface.getByText(/^Vertical scale ×2 · drawn with /).waitFor();
    // A click on the surface picks the nearest stored point, and the caption names that point instead. A click can fall
    // in block 19's gap, where nothing is stored, so a few places on the lower half of the plate are tried in turn.
    const box = await view.boundingBox();
    for (const [x, y] of [[0.5, 0.7], [0.35, 0.65], [0.65, 0.65], [0.5, 0.8]]) {
      await page.mouse.click(box.x + box.width * x, box.y + box.height * y);
      if (!(await caption.innerText()).includes("row 254, line 255")) break;
    }
    await caption.filter({ hasNotText: "row 254, line 255" }).waitFor({ timeout: 2000 });
    // The angle on screen opens in the figure editor, where R draws a preview of it as a surface.
    await surface.getByRole("button", { name: "Draw this angle in R" }).click();
    const editor = page.getByRole("dialog", { name: /^Season difference/ });
    assert.equal(await editor.getByRole("radio", { name: /^Surface/ }).isChecked(), true, `${name}: the editor opens on the surface chart`);
    const turn = Number(await editor.getByRole("spinbutton", { name: "Turn" }).inputValue());
    const tilt = Number(await editor.getByRole("spinbutton", { name: "Tilt" }).inputValue());
    assert.ok(turn >= -180 && turn <= 180 && tilt >= 0 && tilt <= 90, `${name}: the angle is one persp() takes (${turn}, ${tilt})`);
    assert.equal(await editor.getByRole("spinbutton", { name: "Vertical scale" }).inputValue(), "0.7", `${name}: ×2 is twice R's share for the height`);
    await editor.locator(".stage .state").getByText("Preview, not saved", { exact: true }).waitFor();
    await noSidewaysScroll(page, `surface editor ${name}`);
    await capture(page, `surface_editor_${name}`);
    await editor.getByRole("button", { name: "Close the figure editor" }).click();
    await editor.waitFor({ state: "hidden" });
  }
  await noSidewaysScroll(page, `result frames 3D ${name}`);
  await capture(page, `result_3d_${name}`);
  // The R figure is the same numbers as R drew them, with its PDF original.
  await views.getByRole("tab", { name: "R figure" }).click();
  await reader.getByRole("img", { name: "Season difference" }).waitFor();
  await reader.getByText("SVG copy of the R PDF. Checksum matches.", { exact: true }).waitFor();
  await reader.getByRole("button", { name: "Download PDF", exact: true }).waitFor();
  // Back in values, the pick is still marked on its row, and its page is the one shown.
  const picked = (await caption.innerText()).match(/row ([\d,]*\d)/)[1];
  await views.getByRole("tab", { name: "Values" }).click();
  await values.getByRole("button", { name: new RegExp(`^Pick row ${picked}, line `) }).and(values.locator("[aria-pressed=true]")).waitFor();
  await caption.getByRole("button", { name: "Clear", exact: true }).click();
  await caption.waitFor({ state: "detached" });
  assert.equal(await values.locator("[aria-pressed=true]").count(), 0, `${name}: clearing the pick unmarks the row`);
  // The arrow keys on the switch move between views.
  await views.getByRole("tab", { name: "Values" }).focus();
  await page.keyboard.press("ArrowLeft");
  assert.equal(await views.getByRole("tab", { name: "R figure" }).getAttribute("aria-selected"), "true", `${name}: arrows move along the switch`);
  await noSidewaysScroll(page, `result frames ${name}`);
  await capture(page, `result_frame_${name}`);

  // Quick Look: Space on a file opens it larger, the arrows move to the next file, Escape returns to the list.
  const titles = (await list.getByRole("button").allInnerTexts()).map((t) => t.replace(/\s+/g, " ").trim());
  const at = titles.findIndex((t) => t.startsWith("Season difference A results table"));
  await table.focus();
  await page.keyboard.press(" ");
  const look = page.getByRole("dialog", { name: /^Quick Look: Season difference/ });
  await look.waitFor();
  await look.getByText(`${at + 1} of ${titles.length}`, { exact: true }).waitFor();
  await look.getByRole("tablist", { name: "Views of Season difference" }).waitFor();
  await look.getByRole("heading", { name: "Season difference", level: 2 }).waitFor();
  await look.getByText("Step 26, Compare the blocks · Evidence review", { exact: true }).waitFor();
  if (mobile) {
    // Measured once the sheet has risen.
    await look.evaluate((dialog) => Promise.all(dialog.getAnimations().map((a) => a.finished)));
    const sheet = await look.boundingBox();
    assert.ok(Math.abs(sheet.y + sheet.height - page.viewportSize().height) < 2, `${name}: on a phone Quick Look is a sheet from the bottom edge`);
  }
  await noSidewaysScroll(page, `quick look ${name}`);
  await capture(page, `quick_look_${name}`);
  await page.keyboard.press("ArrowRight");
  await look.getByText(`${at + 2} of ${titles.length}`, { exact: true }).waitFor();
  // Keys inside a view stay with it: the switch's arrows change the view, not the file.
  await look.getByRole("tab", { name: "R figure" }).focus();
  await page.keyboard.press("ArrowRight");
  assert.equal(await look.getByRole("tab", { name: "Values" }).getAttribute("aria-selected"), "true", `${name}: the switch takes its own arrows`);
  await look.getByText(`${at + 2} of ${titles.length}`, { exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await look.waitFor({ state: "detached" });
  const focusedOn = (expected) => page.waitForFunction((text) => document.activeElement?.closest("li")?.innerText.replace(/\s+/g, " ").trim() === text,
    expected, { timeout: 2000 }).then(() => true, () => false);
  assert.ok(await focusedOn(titles[at + 1]), `${name}: closing Quick Look returns to the file the reader ended on`);
  await reader.getByRole("tablist", { name: "Views of Season difference" }).getByRole("tab", { selected: true }).waitFor();
}

// The lab is the letterhead: its name, the center beneath it, one quiet line in the rail, and
// every analysis says which lab and project it belongs to and who can open it.
async function labShell(page, mobile, name) {
  await page.goto(home(""), { waitUntil: "domcontentloaded" });
  // Home is the chosen lab's (in one lab, another lab's work never shows unasked). It asks first
  // (Glass Box), then lists what needs the reader, what is working and the lab's projects.
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  const index = page.getByRole("main");
  const ask = index.getByRole("form", { name: "Ask a research question" });
  await ask.getByRole("textbox", { name: "Ask a research question" }).waitFor();
  assert.ok(await ask.getByRole("button", { name: "Continue", exact: true }).isDisabled(), `${name}: Continue waits for a question`);
  await ask.getByText("The agent can work from 3 sets in Example Nutrition Lab; you choose one, several or all of them next.", { exact: true }).waitFor();
  // Needs you lists the items the sidebar counts, each with the action that deals with it, and names the sidebar's
  // number of new items (the page counted 1 waiting project while the bar counted 3 items).
  const waiting = index.getByRole("region", { name: /^Needs you/ });
  const asking = waiting.getByRole("listitem").filter({ hasText: "The agent stopped at decision 3" });
  await asking.getByRole("link", { name: "Review: Test diet response study", exact: true }).waitFor();
  assert.equal(await asking.getByText("New", { exact: true }).count(), 1, `${name}: an unseen item is marked new`);
  const unseenAtStart = await page.evaluate(async () => (await (await fetch("/api/v1/inbox")).json()).items
    .filter((i) => !i.seen_at && i.lab === "open_methods").length);
  assert.ok(unseenAtStart > 1, `${name}: the fixture holds more than one unseen item`);
  await waiting.getByRole("heading", { name: `Needs you ${unseenAtStart}`, exact: true, level: 2 }).waitFor();
  if (!mobile) await page.getByRole("navigation", { name: "Places" }).getByRole("link", { name: `Waiting for you, ${unseenAtStart} new`, exact: true }).waitFor();
  // The paper waiting in another lab's library stays out of this lab's lists and counts; the lab button and the lab
  // menu count it.
  assert.equal(await waiting.getByText(/Example Food Composition Lab/).count(), 0, `${name}: another lab's paper stays off this lab's home`);
  const labButton = page.locator("header.bar").getByRole("button", { name: /^Change lab/ });
  await page.locator("header.bar").getByRole("button", { name: "Change lab. You are in Example Nutrition Lab. One item waits for you in your other labs.", exact: true }).waitFor();
  await labButton.click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Food Composition Lab.* · 1 waiting for you$/ }).waitFor();
  await page.keyboard.press("Escape");
  // Picking another lab in the filter widens the list on purpose; the lab in view starts checked.
  await index.getByRole("button", { name: /^Filter/ }).click();
  const labFacets = page.getByRole("dialog", { name: "Filter projects" });
  assert.ok(await labFacets.getByRole("checkbox", { name: /^Example Nutrition Lab/ }).isChecked(), `${name}: the lab in view starts checked`);
  await labFacets.getByRole("checkbox", { name: /^Example Field Lab/ }).check();
  await labFacets.getByRole("button", { name: "Done" }).click();
  await page.getByRole("heading", { name: "Your research", level: 1 }).waitFor();
  // Working now lists what an agent is working on, with Pause for a reader who may steer it.
  const working = index.getByRole("region", { name: /^Working now/ });
  await working.getByRole("link", { name: "Open Soil moisture sensor comparison", exact: true }).waitFor();
  await working.getByRole("link", { name: "Open Canopy temperature across the season", exact: true }).waitFor();
  await working.getByRole("listitem").filter({ hasText: "Soil moisture sensor comparison" }).getByRole("button", { name: "Pause", exact: true }).waitFor();
  // A waiting project stays in its lab's list, with where it stands.
  const nutrition = index.getByRole("region", { name: /^Example Nutrition Lab/ });
  const diet = nutrition.getByRole("link", { name: /^Test diet response study/ });
  await diet.getByText("The agent stopped and asks you to review what it found (2 analyses finished, 1 did not finish).", { exact: true }).waitFor();
  await diet.getByText("Reviewed by a person: 1 of 2 (1 with changes requested).", { exact: true }).waitFor();
  const shared = nutrition.getByRole("link", { name: /Breakfast fibre and morning fullness/ });
  await shared.getByText("Started by Example colleague", { exact: true }).waitFor();
  await index.getByRole("region", { name: /^Example Field Lab/ }).getByText("Working now: the agent is choosing its next step.", { exact: true }).waitFor();
  assert.equal(await index.getByRole("link", { name: /A colleague's private project/ }).count(), 0, `${name}: a colleague's private project stays unlisted`);
  // Who started a project is a segmented control; the lab and standing filters open behind Filter, each counting what
  // the others leave.
  const who = index.getByRole("radiogroup", { name: "Started by" });
  await who.getByRole("radio", { name: /^By colleagues/ }).check();
  await shared.waitFor();
  assert.equal(await index.getByRole("link", { name: /Test diet response study/ }).count(), 0, `${name}: the colleagues filter leaves out the reader's own projects`);
  await who.getByRole("radio", { name: /^All/ }).check();
  await index.getByRole("button", { name: /^Filter/ }).click();
  const facets = page.getByRole("dialog", { name: "Filter projects" });
  await facets.getByRole("radio", { name: /^Working now/ }).check();
  await index.getByRole("link", { name: /Soil moisture sensor comparison/ }).waitFor();
  assert.equal(await index.getByRole("link", { name: /Breakfast fibre/ }).count(), 0, `${name}: the standing filter narrows the index`);
  await facets.getByRole("button", { name: "Clear the filters" }).click();
  await facets.getByRole("button", { name: "Done" }).click();
  // Clearing the filters returns to the chosen lab alone.
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  await index.getByRole("link", { name: /Breakfast fibre/ }).waitFor();
  assert.equal(await index.getByRole("link", { name: /Soil moisture sensor comparison/ }).count(), 0, `${name}: cleared filters leave other labs out`);
  await index.getByRole("searchbox", { name: "Search projects" }).fill("breakfast fibre");
  await index.getByRole("link", { name: /Breakfast fibre/ }).waitFor();
  assert.equal(await index.getByRole("link", { name: /Test diet response study/ }).count(), 0, `${name}: search narrows the index`);
  await index.getByRole("searchbox", { name: "Search projects" }).fill("");
  await noSidewaysScroll(page, `index ${name}`);
  if (!mobile) {
    // In a short window the filter panel stays inside it and scrolls.
    const size = page.viewportSize();
    await page.setViewportSize({ width: 900, height: 360 });
    await index.getByRole("button", { name: /^Filter/ }).scrollIntoViewIfNeeded();
    await index.getByRole("button", { name: /^Filter/ }).click();
    const box = await page.getByRole("dialog", { name: "Filter projects" }).evaluate((panel) => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(() => {
      const r = panel.getBoundingClientRect(); done({ top: r.top, bottom: r.bottom, height: innerHeight }); }))));
    assert.ok(box.top >= 0 && box.bottom <= box.height, `${name}: the filter panel stays inside a short window (${box.top}..${box.bottom} of ${box.height})`);
    await page.keyboard.press("Escape");
    await page.setViewportSize(size);
  }
  await capture(page, `index_${name}`);
  // A home widened to another lab links to Waiting for you in the labs it counted, and that page narrows back to the
  // lab in view.
  await index.getByRole("button", { name: /^Filter/ }).click();
  const widen = page.getByRole("dialog", { name: "Filter projects" });
  await widen.getByRole("checkbox", { name: /^Example Food Composition Lab/ }).check();
  await widen.getByRole("button", { name: "Done" }).click();
  await page.getByRole("heading", { name: "Your research", level: 1 }).waitFor();
  const wideNeeds = index.getByRole("region", { name: /^Needs you/ });
  await wideNeeds.getByText("Papers for the library of Example Food Composition Lab", { exact: true }).waitFor();
  await wideNeeds.getByRole("link", { name: /Waiting for you$/ }).click();
  await page.waitForURL((u) => u.hash === "#/waiting/labs/open_methods,food_composition");
  const wide = page.locator("section.waiting-page");
  await wide.getByText(/^Example Food Composition Lab · /).waitFor();
  await wide.getByText("Test diet response study: the agent stopped at decision 3", { exact: false }).first().waitFor();
  await wide.getByRole("link", { name: "Only Example Nutrition Lab", exact: true }).click();
  await page.waitForURL((u) => u.hash === "#/waiting");
  await wide.getByText("Test diet response study: the agent stopped at decision 3", { exact: false }).first().waitFor();
  assert.equal(await wide.getByText(/Example Food Composition Lab/).count(), 0, `${name}: Only the lab in view leaves the other lab's paper out`);
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  // The rail holds places: what waits for the reader, each lab, and the views across projects.
  // "Waiting for you" lists what asks for this reader across labs, and a person marks an item as seen.
  await go(page, mobile, /^Waiting for you/);
  const inbox = page.locator("section.waiting-page");
  await inbox.getByRole("heading", { name: "Waiting for you", level: 1 }).waitFor();
  const asked = inbox.locator("ol.items > li").filter({ hasText: "Test diet response study: the agent stopped at decision 3" });
  await asked.getByText("The methods check's words.", { exact: false }).waitFor();
  await inbox.getByRole("heading", { name: /^Results no person has reviewed/ }).waitFor();
  assert.equal(await inbox.getByText(/Breakfast fibre/).count(), 0, `${name}: the waiting place lists only what waits for the reader`);
  assert.equal(await inbox.getByText(/Example Food Composition Lab/).count(), 0, `${name}: another lab's paper stays off this lab's Waiting page`);
  await asked.getByRole("button", { name: "Mark as seen", exact: true }).click();
  await asked.getByText(/ · Seen /).waitFor();
  assert.equal(await asked.getByRole("button", { name: "Mark as seen", exact: true }).count(), 0, `${name}: a seen item offers no second mark`);
  await go(page, mobile, /^Your research/);
  await index.getByRole("link", { name: /Breakfast fibre/ }).waitFor();
  // The mark carries back to the home page: one fewer new item, and that item no longer marked new.
  await waiting.getByRole("heading", { name: `Needs you ${unseenAtStart - 1}`, exact: true, level: 2 }).waitFor();
  assert.equal(await asking.getByText("New", { exact: true }).count(), 0, `${name}: a seen item is no longer marked new`);
  const masthead = page.locator("header.bar");
  await masthead.getByRole("button", { name: /^Change lab\. You are in Example Nutrition Lab\./ }).waitFor();
  await masthead.getByText("Example Research Center", { exact: true }).waitFor();
  // The account says who the reader is once: on wide screens in the account menu, on phones at the foot of the drawer.
  if (mobile) {
    await page.getByRole("button", { name: "Open the sidebar" }).click();
    await page.getByRole("navigation", { name: "Places" }).getByText("runs on enrichedlab", { exact: true }).waitFor();
    await page.getByRole("button", { name: "Close the sidebar" }).first().click();
  } else {
    await masthead.getByRole("button", { name: /^Account: signed in as/ }).click();
    const account = page.locator("#account-menu");
    await account.getByText("runs on enrichedlab", { exact: true }).waitFor();
    assert.equal(await account.getByText("researcher@example.invalid").count(), 1, `${name}: the account names the address once`);
    await page.keyboard.press("Escape");
    // The sidebar holds the five places, with the library's other two under it, and the bar switches Simple and
    // Detailed (the reference application's shell, Glass Box).
    const places = page.getByRole("navigation", { name: "Places" });
    for (const place of ["Your research", "Results", "Library", "Commons", "Shared notes"]) await places.getByRole("link", { name: place, exact: true }).waitFor();
    for (const place of [/^Waiting for you/, /^Working now/]) await places.getByRole("link", { name: place }).waitFor();
    const views = masthead.getByRole("group", { name: "How much to show" });
    assert.equal(await views.getByRole("button", { name: "Simple", exact: true }).getAttribute("aria-pressed"), "true", `${name}: home is the simple view`);
    assert.equal(await places.getByRole("button", { name: "Labs", exact: true }).count(), 0, `${name}: the lab menu is the only lab switcher`);
  }
  // Switching labs changes what the shell shows; the broker still decides what opens.
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Field Lab.*3 projects/ }).click();
  await masthead.getByRole("button", { name: /^Change lab\. You are in Example Field Lab\./ }).waitFor();
  // The list changes with the page, inside its transition.
  await page.getByRole("heading", { name: "Example Field Lab", level: 1 }).waitFor();
  await page.getByRole("main").getByRole("link", { name: /Field sampling plan/ }).waitFor();
  assert.equal(await page.getByRole("main").getByRole("link", { name: /Test diet response study/ }).count(), 0, `${name}: another lab's projects stay out of this lab's list`);
  // The lab's page has its own address: a reload keeps it, and Back returns to all labs.
  await page.waitForURL((url) => url.hash === "#/labs/field_trials", { timeout: 5000 });
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Example Field Lab", level: 1 }).waitFor();
  await page.getByRole("main").getByRole("link", { name: /Field sampling plan/ }).waitFor();
  assert.equal(await page.getByRole("main").getByRole("link", { name: /Test diet response study/ }).count(), 0, `${name}: the reload kept the lab's page`);
  // A lab with no project yet welcomes the reader with what it can work from, read from the broker, under the
  // question field, which stays the page's one primary action; the list and its filters return with the first project.
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.locator("#lab-menu").getByRole("button", { name: /Example Food Composition Lab.*0 projects/ }).click();
  const welcome = page.getByRole("region", { name: /Example Food Composition Lab is ready for its first project/ });
  await welcome.getByText("Simulated food composition tables", { exact: true }).waitFor();
  assert.equal(await welcome.getByText(/No data and method set is registered/).count(), 0, `${name}: the welcome said the lab holds nothing`);
  assert.equal(await page.getByRole("main").getByRole("button", { name: "Continue", exact: true }).count(), 1, `${name}: the question is the welcome's one primary action`);
  await page.getByRole("main").getByText("The agent can work from Simulated food composition tables.", { exact: true }).waitFor();
  await masthead.getByRole("button", { name: /^Change lab/ }).click();
  await page.getByRole("button", { name: /Example Nutrition Lab/ }).click();
  await projectRow(page, "Test diet response study").waitFor();
  // A lab's page lists what waits in that lab.
  await page.getByRole("main").getByRole("region", { name: /^Needs you/ }).getByRole("listitem")
    .filter({ hasText: "An analysis finished and no person has reviewed its current results." }).first().waitFor();
  // A bare analysis address still names its lab and project; a standalone analysis says it has none.
  await page.goto(home(`analyses/${firstTask}`), { waitUntil: "domcontentloaded" });
  const running = page.locator("article .running");
  await running.getByRole("button", { name: "Test diet response study" }).waitFor();
  await running.getByText("Example Nutrition Lab", { exact: true }).waitFor();
  // A project that cannot be read just now is said to be unknown; it never becomes a claim about access.
  const projectRead = `**/api/v1/projects/${main}`;
  await page.route(projectRead, (route) => route.fulfill({ status: 503, contentType: "application/json", body: "{}" }));
  await page.reload({ waitUntil: "domcontentloaded" });
  await running.getByText("Its project could not be looked up just now", { exact: true }).waitFor();
  await running.getByText("Who can open it could not be looked up just now", { exact: true }).waitFor();
  assert.equal(await running.getByText("Only the person who requested it can open it").count(), 0, `${name}: no access claim while the project is unknown`);
  await page.unroute(projectRead);
  await page.reload({ waitUntil: "domcontentloaded" });
  await running.getByRole("button", { name: "Test diet response study" }).waitFor();
  // Where this came from: who asked is the recorded origin, and each unrecorded part is said in words.
  await analysisTab(page, "How it was made");
  const origin = page.locator("section[aria-labelledby='a-lineage']");
  await origin.getByText("The research agent, in decision 1 of a work session you started.", { exact: true }).waitFor();
  await origin.getByText("Summarize each diet and test feature changes.", { exact: true }).waitFor();
  assert.equal(await origin.locator(".row.change-bar").count(), 1, `${name}: only the unrecorded data class is marked`);
  // The rows read in plain words; identifiers, digests and resource figures wait under Technical details.
  assert.equal(await origin.locator(".row").filter({ hasText: /input:\/\/|sha256|[0-9a-f]{12}|CPU/ }).count(), 0, `${name}: lineage rows carry no identifiers`);
  await origin.locator("details.technical summary").click();
  await origin.locator("details.technical dd").filter({ hasText: "agent-sandbox/site" }).first().waitFor();
  await page.goto(home(`analyses/${standaloneTask}`), { waitUntil: "domcontentloaded" });
  await page.locator("article .running").getByText("Not part of a project", { exact: true }).waitFor();
  await page.locator("article .running").getByText("Only the person who requested it can open it", { exact: true }).waitFor();
  await analysisTab(page, "How it was made");
  await origin.getByText("It is not part of a project, so no project recorded its data selection.", { exact: true }).waitFor();
  // A failed lineage read says so and can be retried; it never shows a guessed origin.
  // The page reads lineage version 3 (`?version=3`); the pattern covers the query string.
  const lineageRead = `**/api/v1/tasks/${standaloneTask}/lineage*`;
  await page.route(lineageRead, (route) => route.fulfill({ status: 503, contentType: "application/json", body: "{}" }));
  await page.reload({ waitUntil: "domcontentloaded" });
  await analysisTab(page, "How it was made");
  await origin.getByText("Where this analysis came from could not be read just now.", { exact: false }).waitFor();
  await page.unroute(lineageRead);
  await origin.getByRole("button", { name: "Try again", exact: true }).click();
  await origin.getByText("It is not part of a project, so no project recorded its data selection.", { exact: true }).waitFor();
  // A failed file read keeps the finished status, says so and can be retried.
  const artifacts = `**/api/v1/tasks/${generatedTask}/artifacts`;
  const steps = `**/api/v1/tasks/${generatedTask}/scientific-history?*`;
  await page.route(artifacts, (route) => route.fulfill({ status: 503, contentType: "application/json", body: "{}" }));
  await page.route(steps, (route) => route.fulfill({ status: 503, contentType: "application/json", body: "{}" }));
  await page.goto(home(`projects/${main}/analyses/${generatedTask}`), { waitUntil: "domcontentloaded" });
  await page.getByText("This analysis is recorded as finished, but its files could not be read just now.", { exact: false }).waitFor();
  // With neither its files nor its recorded steps readable, the Files tab is still there and says why it is empty.
  await analysisTab(page, "Files");
  await page.locator("#apanel-files").getByText("The files of this analysis could not be read just now.", { exact: false }).waitFor();
  await analysisTab(page, "Results");
  await page.unroute(artifacts);
  await page.unroute(steps);
  await page.getByRole("button", { name: "Try again", exact: true }).click();
  await page.getByText("Figure 1.", { exact: true }).waitFor();
  // The decision that asked for this analysis cited one library passage for its step; the record of
  // where the analysis came from names the paper right after the plan step.
  await analysisTab(page, "How it was made");
  const cited = page.locator("section[aria-labelledby='a-lineage'] .row.papers");
  await cited.getByText("When the agent planned this analysis it cited a paper from the lab's library.", { exact: true }).waitFor();
  assert.equal((await cited.locator("li").textContent())?.replace(/\s+/g, " ").trim(),
    "Plasma markers after three test diets: an invented methods note, page 2", `${name}: the cited paper and its page`);
  assert.deepEqual(await page.locator("section[aria-labelledby='a-lineage'] .lineage > dl > .row > dt").allTextContents().then((all) => all.slice(0, 3)),
    ["Asked for by", "Plan step", "Papers"], `${name}: the papers follow the plan step`);
  await noSidewaysScroll(page, `lab shell ${name}`);
  await capture(page, `lab_${name}`);
}

async function readProject(page, mobile, name) {
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  await projectRow(page, "Test diet response study").click();
  await page.getByRole("heading", { name: "Test diet response study", level: 1 }).waitFor();
  // One page, no tabs (Glass Box): where the work stands, what changed since the reader last looked, then the agent's
  // own reading, labelled as unchecked.
  assert.equal(await page.getByRole("tablist").count(), 0, `${name}: the project page has no tabs`);
  const summary = page.locator("#section-summary");
  await summary.getByRole("heading", { name: "Waiting for you", level: 2 }).waitFor();
  // The reader marked this request as seen on the waiting page, so the project says who saw it.
  await summary.getByText("Seen by you.", { exact: true }).waitFor();
  await summary.getByText("The agent stopped and is waiting for your review.", { exact: true }).waitFor();
  await summary.getByText("Both steps finished", { exact: true }).waitFor();
  // Since you last looked counts what the records hold after the reader's last look: the answer, both steps and their
  // files and checks.
  const since = page.locator("#section-since");
  await since.getByRole("heading", { name: /^Since you last looked/, level: 2 }).waitFor();
  assert.deepEqual(await since.locator(".since-title").allTextContents(),
    ["It says the results answer part of your question", "2 steps finished", "35 new files", "3 checks passed"], `${name}: what changed since the last look`);
  await since.getByText("Steps 1 and 2. Every file they made matches its checksum.", { exact: true }).waitFor();
  const found = page.locator("#section-found");
  await found.getByRole("heading", { name: "What it found", level: 2 }).waitFor();
  await found.getByText("The agent's reading, not checked by a person.", { exact: true }).waitFor();
  assert.equal(await page.locator("#section-summary, #section-found").getByText(/answered the question|question was answered/i).count(), 0, `${name}: no claim that the question was answered`);
  // The agent's answer and findings: every value is read from the stored digest of the result file, shown briefly
  // with the exact text kept, and named by its file, row and column.
  await found.locator("p.answer-line").getByText("It says the results answer part of your question.").waitFor();
  const findings = found.getByRole("list", { name: "The agent's findings" });
  const values = findings.locator("data.bound");
  await values.nth(3).waitFor();
  assert.deepEqual(await values.evaluateAll((els) => els.map((e) => [e.textContent, e.getAttribute("value")])),
    [["12.38", "12.3791666666667"], ["8.857", "8.85666666666667"], ["3.522", "3.5225"], ["3.079e-8", "3.07933549770212e-08"]],
    `${name}: finding values come from the result tables`);
  assert.match(await values.first().getAttribute("title") ?? "", /^From group_summary\.csv, row 2, column “mean”: 12\.3791666666667\./);
  assert.equal(await findings.locator(".bound.missing").count(), 0, `${name}: every finding value was found`);
  // The colleague asked for changes to the analysis these findings read, so each says so.
  assert.equal(await findings.getByText("A person asked for changes to an analysis this finding reads", { exact: true }).count(), 2,
    `${name}: each finding on the reviewed analysis is marked`);
  // A whole table the agent gave as part of its answer shows the columns it named, in its order, with every
  // row read from the stored digest; a long decimal reads to four significant digits and keeps the file's exact text.
  const answerTable = found.getByRole("region", { name: "Every pairwise comparison between the diets, scroll for more" });
  await answerTable.waitFor();
  assert.deepEqual(await answerTable.locator("thead th").allTextContents(), ["comparison", "diff", "p adj"], `${name}: the answer table's columns`);
  const comparisons = (await readFile(new URL("../fixtures/r/generated/package/results/diet_comparisons.csv", import.meta.url), "utf8"))
    .trim().split("\n").slice(1).map((line) => line.split(",").map((cell) => cell.replace(/^"|"$/g, "")))
    .map(([diff, , , adjusted, comparison]) => [comparison, diff, adjusted]);
  assert.deepEqual(await answerTable.locator("tbody tr").evaluateAll((rows) => rows.map((row) => [...row.cells]
    .map((c) => c.querySelector("data")?.getAttribute("value") ?? c.textContent))), comparisons, `${name}: the answer table's rows are the file's`);
  assert.deepEqual(await answerTable.locator("tbody tr").first().locator("td").allTextContents(), ["diet_a-reference", "3.522", "3.079e-8"],
    `${name}: the answer table reads like the findings`);
  await noSidewaysScroll(page, `answer table ${name}`);
  await found.getByText("3 rows, from step 2, Compare plasma response across the three diets.", { exact: true }).waitFor();
  assert.equal(await found.getByRole("link", { name: "Open the analysis" }).count(), 1, `${name}: the answer table links its analysis`);
  // The colleague's request for changes marks the table as it marks each finding on that analysis.
  assert.equal(await found.getByText("A person asked for changes to an analysis this table reads", { exact: true }).count(), 1,
    `${name}: the answer table on the reviewed analysis is marked`);
  // Every number shows where it came from: in the margin on a wide screen, in a sheet on a phone.
  await findings.getByRole("button", { name: /^12\.38\. From group_summary\.csv/ }).click();
  const note = mobile ? page.getByRole("dialog", { name: "Where it came from" }) : page.getByRole("complementary", { name: "Where it came from" });
  await note.getByText("results/group_summary.csv", { exact: true }).waitFor();
  await note.getByText("12.3791666666667", { exact: true }).waitFor();
  await note.getByText("Step 2: Compare plasma response across the three diets. R code the agent wrote, in a reviewed template.", { exact: true }).waitFor();
  await note.getByText("The file matches the package's checksum list.", { exact: true }).waitFor();
  await capture(page, `project_source_${name}`);
  if (mobile) await note.getByRole("button", { name: "Close" }).click();
  else {
    await note.getByRole("button", { name: "Back to about this answer" }).click();
    await note.getByRole("heading", { name: "About this answer" }).waitFor();
  }
  // About this answer names the data, methods, checks and reviewers; below 1,360 pixels it is a section of the page.
  const about = mobile ? page.locator("section.about") : note;
  await about.getByText("Diet study", { exact: false }).first().waitFor();
  await about.getByText("Every result file matches its recorded checksum", { exact: true }).waitFor();
  await about.getByText("Changes requested by Example colleague", { exact: true }).waitFor();
  // The agent's notes sit one level down, labelled as its own.
  await page.locator("#section-findings").getByText("The retained input has no preparation receipt, so the summary may not be reproducible.").waitFor();
  // How it got here: each step on one line with its method, files, checks and review; the one not reviewed says so.
  const first = projectStep(page, "Summarize each diet and test feature changes");
  await first.locator(".story-detail").getByText("Evidence review · 3 figures and 3 tables · Checks passed · Not yet reviewed by a person", { exact: true }).waitFor();
  await projectStep(page, "Compare plasma response across the three diets").locator(".story-detail").getByText(/Changes requested by Example colleague$/).waitFor();
  await projectStep(page, "Evidence review").getByText("Did not finish", { exact: true }).waitFor();
  // A step opens in place: its saved history labeled by what wrote each change, its checks and its
  // figures, the R PDF drawn from its own bytes.
  await openStep(page, "Summarize each diet and test feature changes");
  const moments = (title) => projectStep(page, title).locator(".step-history > li");
  for (const words of ["You added it", "Its analysis started", "Its analysis finished"])
    await moments("Summarize each diet and test feature changes").filter({ has: page.locator("span", { hasText: new RegExp(`^${words}$`) }) }).waitFor();
  await first.getByText("Methods check agreed", { exact: true }).waitFor();
  await first.getByText("Result files intact", { exact: true }).waitFor();
  await openStep(page, "Compare plasma response across the three diets");
  await moments("Compare plasma response across the three diets").filter({ has: page.locator("span", { hasText: /^The agent planned it$/ }) }).waitFor();
  assert.equal(await moments("Compare plasma response across the three diets").count(), 1, `${name}: a step shows only the changes recorded for it`);
  const plates = first.getByRole("list", { name: "Figures from Summarize each diet and test feature changes" });
  await plates.getByRole("button").last().scrollIntoViewIfNeeded();
  await page.waitForFunction(() => [...document.querySelectorAll(".plates canvas")].some((c) => c.width > 0));
  await page.waitForFunction(() => [...document.querySelectorAll(".plates img")].filter((i) => i.complete && i.naturalWidth > 0).length >= 2);
  await noSidewaysScroll(page, `project ${name}`);
  await capture(page, `project_${name}`);
  // Scrolled past the status, a compact line under the bar keeps the standing and its action; the action brings the
  // answer back into view.
  const pinned = page.locator(".pin.shown");
  await pinned.getByRole("button", { name: "Review the answer", exact: true }).click();
  await page.waitForFunction(() => { const top = document.getElementById("found-title")?.getBoundingClientRect().top ?? -1; return top >= 0 && top < innerHeight / 2; });
  // The full record reads in order: the scientist's entries, the agent's decisions and the system's records.
  await page.locator("#section-record > summary").click();
  const notebook = page.locator("#section-notebook");
  await notebook.getByText("2 replies from the planning model were rejected").waitFor();
  await notebook.getByText("Access changed: Example colleague can now review").waitFor();
  await notebook.getByText("The retained input has no preparation receipt, so the summary may not be reproducible.").first().waitFor();
  const sources = page.getByRole("group", { name: "Show entries from" });
  await sources.getByRole("button", { name: /Research agent/ }).click();
  await page.getByText("2 replies from the planning model were rejected").waitFor({ state: "hidden" });
  await page.getByText("Research agent started an analysis").first().waitFor();
  await sources.getByRole("button", { name: /Everything/ }).click();
  // A plate opens its analysis at the same figure, and the figure leads to the exact code and inputs that made it.
  // The live service is checked over the network, so the figures above the chosen one finish drawing after the page moves.
  const figureFiles = "**/contents/file?*";
  await page.route(figureFiles, async (route) => { await new Promise((done) => setTimeout(done, 1500)); await route.continue().catch(() => {}); });
  await plates.getByRole("button", { name: /^Open figure 4, Response by diet/ }).click();
  await page.getByRole("heading", { name: "Summarize each diet and test feature changes", level: 1 }).waitFor();
  const chosen = page.locator("figure.figure.change-bar");
  await chosen.waitFor();
  assert.match(await chosen.getAttribute("data-path") ?? "", /results\/response_by_diet/, `${name}: the analysis opens at the figure that was chosen`);
  // It stays in view while the figures above it finish drawing and grow past their placeholders.
  await page.waitForFunction(() => {
    const figures = [...document.querySelectorAll("figure.figure[data-path]")];
    const index = figures.findIndex((f) => f.classList.contains("change-bar"));
    return index >= 0 && figures.slice(0, index + 1).every((f) => !f.querySelector(".placeholder"));
  });
  await page.waitForTimeout(1200);
  const placed = await chosen.locator(".plate").evaluate((plate) => {
    const box = plate.getBoundingClientRect();
    const top = document.querySelector("header.bar")?.getBoundingClientRect().bottom ?? 0;
    const room = innerHeight - top;
    const visible = Math.max(0, Math.min(box.bottom, innerHeight) - Math.max(box.top, top));
    return { fits: box.height <= room, shown: visible / Math.min(box.height, room) };
  });
  assert.ok(placed.shown >= (placed.fits ? 0.98 : 0.9), `${name}: the chosen figure is in view once the figures above it are drawn (${placed.shown.toFixed(2)} of it shows)`);
  await page.unroute(figureFiles);
  await page.getByRole("region", { name: /Condition summary/ }).getByRole("columnheader", { name: "mean", exact: true }).waitFor();
  await page.getByText("All 18 files match the checksums recorded when the analysis ran.").first().waitFor();
  // "Show how this was made" opens the tab with the method and marks the files that made the figure.
  await chosen.getByRole("button", { name: "Show how this was made" }).click();
  // The link hid the tab it sat in, so keyboard focus moves to what it opened.
  assert.ok(await page.evaluate(() => !!document.activeElement?.closest("#apanel-how")), `${name}: focus moves to the method the link opened`);
  await page.locator("#apanel-how").getByText(/per-diet count, mean, median and standard deviation/).first().waitFor();
  const marked = page.locator("section[aria-labelledby='a-method'] li.change-bar");
  await marked.filter({ hasText: "code/analysis.R" }).waitFor();
  assert.ok(await marked.filter({ hasText: "input/measurements.csv" }).count() === 1, "the retained input is marked with the code");
  await page.getByText("Synthetic demonstration analysis for the research workspace preview.", { exact: false }).first().waitFor();
  await noSidewaysScroll(page, `analysis from a plate ${name}`);
  // Back to the project for the controls that follow.
  await page.locator("article .running").getByRole("button", { name: "Test diet response study" }).click();
  await page.getByRole("heading", { name: "Test diet response study", level: 1 }).waitFor();
}

// Model-written R: the admitted block, both admission checks, the isolation probe and the
// unvalidated mark on its figure, read from the broker record and the retained package.
// A later decision that says the results do not answer the question, with no findings, becomes
// the current answer after reload; the earlier findings stay as dated history and never read as the answer.
async function correctedAnswer(page, name) {
  const turnsRead = "**/api/v1/research-runs/*/turns*";
  await page.route(turnsRead, async (route) => {
    // The workspace reads conditionally (live data); a body this route edits is asked for in full and leaves without
    // the broker's ETag, so the workspace never keeps the edited copy under it.
    const asked = { ...route.request().headers() };
    delete asked["if-none-match"];
    const response = await route.fetch({ headers: asked });
    const body = await response.json();
    const last = body.turns.at(-1);
    if (last?.record?.proposal?.answer === "partly_answered") {
      const later = structuredClone(last);
      later.turn = last.turn + 1;
      later.created_at = new Date(Date.parse(last.created_at) + 3600_000).toISOString();
      later.record.proposal = { ...later.record.proposal, answer: "not_answered", findings: [],
        summary: "The comparison does not settle the question once the second diet is excluded." };
      body.turns.push(later);
    }
    const sent = { ...response.headers() };
    delete sent.etag;
    delete sent["content-length"];
    await route.fulfill({ response, json: body, headers: sent });
  });
  const summary = page.locator("#section-found");
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  for (const pass of ["open", "reload"]) {
    if (pass === "reload") await page.reload({ waitUntil: "domcontentloaded" });
    const line = summary.locator("p.answer-line");
    await line.getByText("It says the results do not answer your question.").waitFor();
    assert.doesNotMatch(await line.innerText(), /answer part of your question/, `${name} (${pass}): the older answer is not shown as current`);
    assert.equal(await summary.getByRole("list", { name: "The agent's findings", exact: true }).count(), 0,
      `${name} (${pass}): no findings stand for the newer answer`);
    const earlier = summary.locator("details.earlier-answers");
    await earlier.locator("summary").click();
    await earlier.getByText("The agent's newer answer above replaces these findings.", { exact: true }).waitFor();
    await earlier.getByText(/It says the results answer part of your question\./).waitFor();
    const values = earlier.locator("data.bound");
    await values.nth(3).waitFor();
    assert.deepEqual(await values.evaluateAll((els) => els.map((e) => e.getAttribute("value"))),
      ["12.3791666666667", "8.85666666666667", "3.5225", "3.07933549770212e-08"], `${name} (${pass}): earlier values still come from the result tables`);
  }
  await capture(page, `corrected_answer_${name}`);
  await page.unroute(turnsRead);
  await page.reload({ waitUntil: "domcontentloaded" });
  await summary.locator("p.answer-line").getByText("It says the results answer part of your question.").waitFor();
}

// A decision's exact input opens in the notebook for a lab member, is checked against its recorded
// SHA-256 in the browser, switches to the methods check's input, and is never offered as a download.
async function decisionInput(page, name) {
  // The full record opens from its address; the tabbed page's "notebook" address now opens the steps.
  await page.goto(home(`projects/${main}/record`), { waitUntil: "domcontentloaded" });
  assert.ok(await page.locator("#section-record").evaluate((d) => d.open), `${name}: the record's address opens the record`);
  const notebook = page.locator("#section-notebook");
  const entry = notebook.locator("li.entry").filter({ hasText: "Research agent stopped for your review" }).first();
  await entry.getByRole("button", { name: "What the agent was given" }).click();
  await entry.getByText(/Checked in your browser: this is the exact text the decision recorded/).waitFor();
  // A plain summary of the same text comes first; the full text opens on request.
  const plain = entry.getByLabel("Summary of what the research agent was given");
  assert.match(await plain.innerText(), /Your question\s+How does plasma response differ between the reference diet and the two test diets\?/, `${name}: the summary quotes the question`);
  assert.match(await plain.innerText(), /Analyses so far\s+1 analysis: 1 finished/, `${name}: the summary counts the analyses`);
  assert.match(await plain.innerText(), /Papers from the lab's library\s+1 passage from 1 paper/, `${name}: the summary counts the passages by paper`);
  assert.equal(await entry.getByLabel("What the research agent was given", { exact: true }).count(), 0, `${name}: the full text waits to be asked for`);
  await entry.getByRole("button", { name: "Show the full text" }).click();
  const shown = entry.getByLabel("What the research agent was given", { exact: true });
  assert.match(await shown.innerText(), /"context_schema_version": 7/, `${name}: the lead's input is shown formatted`);
  await entry.getByLabel("Show the exact text as sent").check();
  assert.match(await shown.innerText(), /^\{"context_schema_version":7,/, `${name}: the exact text is the compact JSON the model received`);
  await entry.getByRole("button", { name: "Methods check", exact: true }).click();
  assert.match(await entry.getByLabel("Summary of what the methods check was given").innerText(),
    /The research agent's proposal\s+Stop and ask for your review; it says the results answer part of your question/, `${name}: the methods check's summary leads with the proposal`);
  const review = entry.getByLabel("What the methods check was given", { exact: true });
  await review.waitFor();
  assert.match(await review.innerText(), /"answer":"partly_answered"/, `${name}: the methods check's input is its own`);
  assert.equal(await entry.locator(".decision-input a[download], .decision-input a[href]").count(), 0, `${name}: no download for the kept input`);
  await noSidewaysScroll(page, `decision input ${name}`);
  await capture(page, `decision_input_${name}`);
  await entry.getByRole("button", { name: "Hide what the agent was given" }).click();
}

async function readGeneratedAnalysis(page, name) {
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await projectRow(page, "Test diet response study").click();
  await page.getByRole("heading", { name: "Test diet response study", level: 1 }).waitFor();
  await (await openStep(page, "Compare plasma response across the three diets")).getByRole("link", { name: "Open the analysis", exact: true }).click();
  await page.getByRole("heading", { name: "Compare plasma response across the three diets", level: 1 }).waitFor();
  await page.getByText("Model-written R in the reviewed template. Unvalidated until reviewed.").first().waitFor();
  // The kind of data recorded when its copy was sealed is named on the Data row, with no gap left for it.
  await analysisTab(page, "How it was made");
  await page.locator("section[aria-labelledby='a-lineage']")
    .getByText("Public reference data, recorded when the platform sealed the copy this analysis read.", { exact: true }).waitFor();
  const code = page.locator("section[aria-labelledby='a-code']");
  await code.getByRole("heading", { name: "Model-written code" }).waitFor();
  await code.getByText("Treat the results as unvalidated until you have read the code.").waitFor();
  await code.getByText(/The checks below\s+are the template's own records; an independent review of them is still open\./).waitFor();
  assert.equal(await code.getByText(/wrote every file/).count(), 0, "no claim that the sandbox wrote the evidence");
  for (const check of ["Accepted by the broker's check", "Accepted again by the template",
    "No internet: a test connection to an outside address was refused", "No cluster control: the cluster API could not be reached",
    "No name lookups: an internet name could not be resolved", "No cluster credential inside the sandbox",
    "The staged input could not be changed", "System files could not be changed",
    // Template v2: the block ran in a separate process and, by the template's own record, its tables followed a changed input.
    "Ran in a separate R process; every function its code names is on the template's allowlist", "Its tables changed when the input changed, by the template's count"])
    await code.getByText(check, { exact: true }).waitFor();
  await code.getByText(/Rerun on a copy with 3 rows removed and small noise on measured values: the template counted \d+ of \d+ numbers as changed\./).waitFor();
  assert.equal(await code.locator(".status.failed").count(), 0, "every recorded check passed");
  await code.getByText("The block ran and returned its tables and figures.").waitFor();
  await code.getByText(/generated\/block\.R · 32 lines · checksum verified/).waitFor();
  await code.getByText("analyze <- function(data) {", { exact: false }).first().waitFor();
  const [download] = await Promise.all([page.waitForEvent("download"), code.getByRole("button", { name: "Download", exact: true }).click()]);
  assert.equal(download.suggestedFilename(), "block.R");
  await noSidewaysScroll(page, `generated analysis code ${name}`);
  await analysisTab(page, "Results");
  await page.getByText("Figure 1.", { exact: true }).waitFor();
  await noSidewaysScroll(page, `generated analysis ${name}`);
  await capture(page, `generated_${name}`);
}

// A configured recipe: the same analysis read as one the agent configured instead of wrote. The page says
// so wherever the code is described and lists the chosen settings from the broker's record.
async function readConfiguredAnalysis(page, name) {
  const response = await fetch(new URL("/api/v1/fixture/configured", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(response.status, 200);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Compare plasma response across the three diets", level: 1 }).waitFor();
  await page.getByText("A configured recipe in the reviewed template, with settings the agent chose. Unvalidated until reviewed.").first().waitFor();
  assert.equal(await page.getByText("Model-written R in the reviewed template. Unvalidated until reviewed.").count(), 0);
  await analysisTab(page, "How it was made");
  const code = page.locator("section[aria-labelledby='a-code']");
  await code.getByRole("heading", { name: "The recipe and its settings" }).waitFor();
  await code.getByText(/This is a configured recipe\. The research agent chose the settings below and wrote none of the code\./).waitFor();
  assert.equal(await code.getByText("The research model wrote this R code.", { exact: false }).count(), 0);
  const settings = code.locator("dl.settings");
  assert.deepEqual(await settings.locator("dt").allTextContents(), ["Groups", "Interval level", "Resamples per group", "Statistic"]);
  assert.deepEqual(await settings.locator("dd").allTextContents(), ["each diet", "95%", "10,000", "mean"]);
  await page.getByText(/a configured recipe run with settings the agent chose; the agent wrote no code\./).first().waitFor();
  await page.getByText(/The configured recipe with its settings filled in \([\d.]+ KB\), checked by the platform before it ran\./).first().waitFor();
  assert.equal(await page.getByText(/^Model-written code \(/).count(), 0, "no model-written claim for a configured recipe");
  await noSidewaysScroll(page, `configured analysis ${name}`);
  await capture(page, `configured_${name}`);
}

// A database query: the same analysis read as a read-only query the agent wrote. The page shows the
// query beside its source, what was checked before it ran and what came back; no model-written code is claimed.
async function readQueryAnalysis(page, name) {
  const on = await fetch(new URL("/api/v1/fixture/query", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(on.status, 200);
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Compare plasma response across the three diets", level: 1 }).waitFor();
  await analysisTab(page, "How it was made");
  const query = page.locator("section[aria-labelledby='a-query']");
  await query.getByRole("heading", { name: "The database query" }).waitFor();
  await query.getByText(/The research agent wrote this query\. It read Example reference data \(Example release\) without changing it/).waitFor();
  await query.getByText("Treat the results as unvalidated until you have read the query.").waitFor();
  for (const check of ["Admitted by the platform's query check", "Read only, in one consistent view of the database",
    "The database held exactly the recorded release of this source"]) await query.getByText(check, { exact: true }).waitFor();
  await query.getByText("It returned 3 rows with 2 columns.").waitFor();
  await query.getByText(/ORDER BY fn\.amount DESC, f\.description/).first().waitFor();
  assert.equal(await page.locator("section[aria-labelledby='a-code']").count(), 0, `${name}: no model-written code for a query`);
  await page.getByText(/a read-only database query the agent wrote, checked before it ran; no code ran\./).first().waitFor();
  await page.getByText("A read-only query of 3 tables, at most 3 rows, checked by the platform before it ran.").first().waitFor();
  // Where the data came from names the source and its kind of data; nothing in the query's history is unrecorded.
  const lineage = page.locator("section[aria-labelledby='a-lineage']");
  await lineage.getByText("Example reference data. Public reference data read from one pinned snapshot.", { exact: true }).waitFor();
  await lineage.getByText("Every part of this analysis's history is recorded.", { exact: true }).waitFor();
  assert.equal(await lineage.locator(".row.change-bar").count(), 0, `${name}: no unrecorded part for a query`);
  const [download] = await Promise.all([page.waitForEvent("download"), query.getByRole("button", { name: "Download", exact: true }).click()]);
  assert.equal(download.suggestedFilename(), "query.sql");
  await page.getByText("The package holds the query, the parameters, the results and the checksums.", { exact: false }).first().waitFor();
  await noSidewaysScroll(page, `query analysis ${name}`);
  await capture(page, `query_${name}`);
  const off = await fetch(new URL("/api/v1/fixture/query?on=false", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(off.status, 200);
  await page.reload({ waitUntil: "domcontentloaded" });
  await analysisTab(page, "How it was made");
  await page.locator("section[aria-labelledby='a-code']").getByRole("heading", { name: "The recipe and its settings" }).waitFor();
}

// A figure drawn by R from a result table (Glass Box results rule 4). The reader chooses settings while R
// draws previews that make no version, sees a value the editor cannot send named in plain words and what the render
// service changed, gives each group a colour, checks the colours as seen with colour blindness and the drawing at print
// size, keeps two versions, opens one, marks it for the manuscript, sets it beside the other, edits it into a third the
// service refuses, and finds the figure once on its source analysis and on the project. The figure method is never
// offered as a method to run.
async function makeFigure(page, mobile, name) {
  const versions = async () => (await (await fetch(new URL(`/api/v1/tasks/${firstTask}/figures`, target))).json()).versions;
  const settled = async (count) => {
    for (let i = 0; i < 60; i++) {
      const list = await versions();
      if (list.length === count && list.every((v) => ["completed", "failed"].includes(v.status))) return list;
      await page.waitForTimeout(250);
    }
    throw new Error(`${name}: figure version ${count} did not finish`);
  };
  await page.goto(home(`projects/${main}/analyses/${firstTask}`), { waitUntil: "domcontentloaded" });
  await page.locator("section.table-view", { hasText: "Condition summary" }).getByRole("button", { name: "Make a figure" }).click();
  const dialog = page.getByRole("dialog");
  const scatter = dialog.getByRole("radio", { name: /^Scatter plot/ });
  await scatter.waitFor();
  assert.equal(await scatter.isChecked(), true, `${name}: the first chart the table can draw starts chosen`);
  // Numbers start where the chart measures, and R draws a preview of the first settings.
  assert.equal(await dialog.getByRole("combobox", { name: "Across" }).inputValue(), "n");
  assert.equal(await dialog.getByRole("combobox", { name: "Up" }).inputValue(), "mean");
  const state = dialog.locator(".stage .state");
  await state.getByText("Preview, not saved", { exact: true }).waitFor();
  await dialog.getByRole("radio", { name: /^Bar chart/ }).check();
  await dialog.getByRole("combobox", { name: "Values" }).selectOption("mean");
  await dialog.getByRole("textbox", { name: "Title", exact: true }).fill("Mean response by diet");
  await dialog.getByRole("textbox", { name: "Horizontal axis title" }).fill("Diet");
  await dialog.getByRole("textbox", { name: "Vertical axis title" }).fill("Mean response");
  await dialog.getByText("Print, 300").click();
  // A width outside the service's range is named once the typing pauses, and nothing is drawn or kept.
  await dialog.getByRole("spinbutton", { name: "Width in mm" }).fill("600");
  await dialog.getByText("Width must be between 76.2 and 508 mm.").waitFor();
  await state.getByText("Preview of earlier settings, not saved", { exact: true }).waitFor();
  await dialog.getByText("Full width, 180 mm").click();
  await dialog.getByText("More settings").click();
  await dialog.getByRole("textbox", { name: "Vertical axis range" }).fill("20, 0");
  // Each group of the category column can take one of Okabe and Ito's colours.
  const colours = dialog.getByRole("group", { name: "Colours per group" });
  await colours.locator(".colour-row select").first().selectOption({ label: "Sky blue" });
  await dialog.getByText("R changed or left out:").waitFor();
  await dialog.getByText("Vertical axis range was given as maximum then minimum and was reordered").waitFor();
  await state.getByText("Preview, not saved", { exact: true }).waitFor();
  assert.equal(await dialog.locator(".problems").count(), 0, `${name}: a fixed value clears its problem`);
  assert.equal((await versions()).length, 0, `${name}: previews make no version`);
  // The colour vision views and the print size change how the drawing is shown, never the figure.
  await dialog.getByRole("combobox", { name: "Colour vision" }).selectOption({ label: "Deuteranopia (red-green)" });
  await dialog.getByText("A simulation of deuteranopia (red-green) for checking the colours. The figure does not change.").waitFor();
  assert.match(await dialog.locator(".paper img").evaluate((img) => getComputedStyle(img).filter), /^url\(/, `${name}: the simulation is a filter on the drawing`);
  await dialog.getByRole("checkbox", { name: "Print size" }).check();
  await dialog.getByText(/^10 mm at print size\. The drawing is 180 mm wide on paper/).waitFor();
  await noSidewaysScroll(page, `figure editor ${name}`);
  await capture(page, `figure_editor_${name}`);
  await dialog.getByRole("checkbox", { name: "Print size" }).uncheck();
  await dialog.getByRole("combobox", { name: "Colour vision" }).selectOption({ label: "Typical colour vision" });
  await dialog.getByRole("button", { name: "Keep this version" }).click();
  await state.getByText("Kept version", { exact: true }).waitFor();
  await dialog.getByText(/Kept as its own version, with where it came from/).waitFor();
  await settled(1);
  // A change after keeping draws a new preview; keeping it makes the next version.
  await dialog.getByRole("textbox", { name: "Vertical axis title" }).fill("Mean plasma response");
  await state.getByText("Preview, not saved", { exact: true }).waitFor();
  await dialog.getByRole("button", { name: "Keep this version" }).click();
  await state.getByText("Kept version", { exact: true }).waitFor();
  const [second] = await settled(2);
  assert.equal(second.title, "Mean response by diet");
  await dialog.getByRole("button", { name: "Open this version" }).click();
  await page.getByRole("heading", { name: "Mean response by diet", level: 1 }).waitFor();
  const record = page.locator("section[aria-labelledby='a-figure']");
  await record.getByRole("button", { name: "Edit this figure" }).waitFor();
  // The source is named by its plan step's title, as its own page is, with one full stop.
  assert.equal(await record.locator("p.summary").innerText(), "Bar chart of Condition summary, 3 rows, from Summarize each diet and test feature changes.");
  await record.getByText("Version 2, this page").waitFor();
  await record.getByRole("link", { name: "Version 1" }).waitFor();
  for (const [label, value] of [["Categories", "condition"], ["Values", "mean"], ["Vertical axis title", "Mean plasma response"], ["Resolution", "300 dots per inch"], ["Width", "180 mm"]]) {
    await record.locator(".row", { hasText: label }).getByText(value, { exact: true }).waitFor();
  }
  assert.match(await record.locator(".row", { hasText: "Colours per group" }).innerText(), /sky blue/, `${name}: the colours are named in words`);
  // One drawn version per figure is marked as the one in the manuscript.
  await record.getByText("No version of this figure is marked as the one in the manuscript.", { exact: true }).waitFor();
  await record.getByRole("button", { name: "Mark this version for the manuscript" }).click();
  await record.getByText(/^This version is the one in the manuscript, marked by you on /).waitFor();
  await record.locator("li.current").getByText("In the manuscript", { exact: true }).waitFor();
  // Two versions side by side: both drawings, and the one setting that differs marked.
  await record.getByRole("button", { name: "Compare two versions" }).click();
  const compare = record.getByRole("region", { name: "Two versions side by side" });
  await compare.getByRole("img", { name: "Version 2, this page, drawn by R" }).waitFor();
  await compare.getByRole("img", { name: "Version 1, drawn by R" }).waitFor();
  await compare.getByText("One setting differs: Vertical axis title.", { exact: true }).waitFor();
  await compare.getByRole("checkbox", { name: "Only the settings that differ" }).check();
  assert.deepEqual((await compare.locator("tbody tr").allInnerTexts()).map((t) => t.replace(/\s+/g, " ").trim()),
    ["Differs: Vertical axis title Mean plasma response Mean response"], `${name}: only the difference is listed`);
  await noSidewaysScroll(page, `figure compare ${name}`);
  await capture(page, `figure_compare_${name}`);
  await analysisTab(page, "How it was made");
  const lineage = page.locator("section[aria-labelledby='a-lineage']");
  await lineage.getByText("Condition summary, a table from an earlier analysis in this project.", { exact: true }).waitFor();
  await lineage.getByText("The reviewed R render service, release 0.6.0, pinned to an exact build.", { exact: true }).waitFor();
  await lineage.getByText("Drawn by the reviewed R render service.", { exact: true }).waitFor();
  await analysisTab(page, "Results");
  await page.locator("figure.figure").getByText("Mean response by diet").first().waitFor();
  await noSidewaysScroll(page, `figure version ${name}`);
  await capture(page, `figure_version_${name}`);
  await record.getByRole("button", { name: "Edit this figure" }).click();
  await dialog.getByText("A new version of this figure from").waitFor();
  assert.equal(await dialog.getByRole("radio", { name: /^Bar chart/ }).isChecked(), true, `${name}: an edit starts from the version's chart`);
  assert.equal(await dialog.getByRole("textbox", { name: "Title", exact: true }).inputValue(), "Mean response by diet");
  await state.getByText("Preview, not saved", { exact: true }).waitFor();
  await dialog.getByRole("radio", { name: /^Scatter plot/ }).check();
  await dialog.getByRole("combobox", { name: "Across" }).selectOption("n");
  await dialog.getByRole("combobox", { name: "Up" }).selectOption("condition");
  // A refused preview shows the broker's words and marks the setting whose column the refusal names; keeping the
  // settings anyway keeps a version that was not drawn.
  await dialog.getByText("No numbers this figure can draw in condition.", { exact: true }).waitFor();
  assert.equal(await dialog.getByRole("combobox", { name: "Up" }).getAttribute("aria-invalid"), "true", `${name}: the refused column's setting is marked`);
  assert.equal(await dialog.getByRole("combobox", { name: "Across" }).getAttribute("aria-invalid"), null, `${name}: other settings are not marked`);
  await dialog.getByRole("button", { name: "Keep this version" }).click();
  await dialog.getByText("Not drawn. No numbers this figure can draw in condition.", { exact: true }).waitFor();
  await dialog.getByText("Nothing was drawn. The reason is below.").waitFor();
  const [refused] = await settled(3);
  await dialog.getByRole("button", { name: "Close the figure editor" }).click();
  // A version that was not drawn says so in the editor's words, and its record names no missing history.
  await page.goto(home(`projects/${main}/analyses/${refused.task_id}`), { waitUntil: "domcontentloaded" });
  const refusedRecord = page.locator("section[aria-labelledby='a-figure']");
  await refusedRecord.getByText("Version 3, this page").waitFor();
  assert.match(await refusedRecord.locator("li.current").innerText(), /Not drawn/, `${name}: a refused version reads as not drawn`);
  await refusedRecord.getByText(/^Version 2 is the one in the manuscript, marked by you on /).waitFor();
  assert.equal(await refusedRecord.getByRole("button", { name: /Mark this version/ }).count(), 0, `${name}: a version that was not drawn cannot be marked`);
  await page.locator("header.head").getByText("Not drawn", { exact: true }).waitFor();
  await analysisTab(page, "How it was made");
  const refusedLineage = page.locator("section[aria-labelledby='a-lineage']");
  await refusedLineage.getByText("None: the figure was not drawn.", { exact: true }).waitFor();
  await refusedLineage.getByText("Nothing to review: the figure was not drawn.", { exact: true }).waitFor();
  await refusedLineage.getByText("Every part of this analysis's history is recorded.", { exact: true }).waitFor();
  // The source analysis lists the figure once, by the version in the manuscript, and the project lists it once.
  await page.goto(home(`projects/${main}/analyses/${firstTask}`), { waitUntil: "domcontentloaded" });
  await page.getByText("Figures made from these results").waitFor();
  await page.getByRole("link", { name: "Mean response by diet" }).waitFor();
  await page.getByText(/^Bar chart of Condition summary · 3 versions · .* · the version in the manuscript$/).waitFor();
  await page.goto(home(`projects/${main}/results`), { waitUntil: "domcontentloaded" });
  const entries = page.locator("#section-steps .story-title", { hasText: "Mean response by diet" });
  await entries.first().waitFor();
  assert.equal(await entries.count(), 1, `${name}: a figure drawn again is listed once`);
  await projectStep(page, "Mean response by diet").getByText(/^Figure drawn by R from Summarize each diet and test feature changes/).waitFor();
  // The figure method is listed for its name only; it is not a method to run.
  await go(page, mobile, "Results");
  await page.getByRole("button", { name: "Run one method" }).click();
  const methods = await page.getByRole("dialog").getByRole("combobox", { name: "Method" }).locator("option").allTextContents();
  assert.ok(methods.length > 0 && !methods.includes("Figure from a result table"), `${name}: the figure method is not offered to run`);
  await page.getByRole("dialog").getByRole("button", { name: "Cancel" }).click();
}

// A person's review: the reader records a decision on an analysis as it stands. The analysis page, its record of
// where it came from and the project report show the newest one with its reviewer; an earlier one stays listed.
async function reviewAnalysis(page, name) {
  await page.goto(home(`projects/${main}/analyses/${firstTask}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Summarize each diet and test feature changes", level: 1 }).waitFor();
  await analysisTab(page, "Review");
  const review = page.locator("section[aria-labelledby='a-review']");
  await review.getByText("No one has reviewed this analysis yet.", { exact: true }).waitFor();
  await review.getByText("It is not statistical validation, and it never changes a result.", { exact: false }).waitFor();
  // The record of where it came from links to the review, which opens the Review tab.
  await analysisTab(page, "How it was made");
  const origin = page.locator("section[aria-labelledby='a-lineage']");
  await origin.getByRole("button", { name: "Review this analysis", exact: true }).click();
  assert.equal(await page.getByRole("tab", { name: /^Review/ }).getAttribute("aria-selected"), "true", `${name}: the review link opens the Review tab`);
  const form = review.locator("form");
  // A request for changes needs a note, and the page says so before anything is sent.
  await form.getByRole("radio", { name: /^Needs changes/ }).check();
  await form.getByRole("button", { name: "Record your review", exact: true }).click();
  await form.getByText("Say what should change.", { exact: true }).waitFor();
  const note = `Compare each test diet with the control in the ${name} check.`;
  await form.getByLabel("Note", { exact: true }).fill(note);
  await form.getByRole("button", { name: "Record your review", exact: true }).click();
  await page.getByText("Your review is saved with this analysis.").first().waitFor();
  const list = review.getByRole("list", { name: "Reviews of this analysis, newest first" });
  await list.getByText("Changes requested by you", { exact: true }).waitFor();
  await list.getByText(note, { exact: true }).waitFor();
  // Acceptance needs no note. A later review is added above the earlier one and leads the page.
  await form.getByRole("radio", { name: /^Accept/ }).check();
  await form.getByRole("button", { name: "Record your review", exact: true }).click();
  await list.locator("li").first().getByText("Accepted by you", { exact: true }).waitFor();
  assert.equal(await list.locator("li").count(), 2, `${name}: a later review never replaces an earlier one`);
  await list.locator("li").first().getByText("It covers the analysis's 2 result files as they are now, by their checksums.", { exact: true }).waitFor();
  await page.locator("article header .now").getByText("Accepted by you", { exact: true }).waitFor();
  await analysisTab(page, "How it was made");
  await origin.getByText(/^Accepted by you on /).waitFor();
  await origin.getByText("1 earlier review.", { exact: true }).waitFor();
  await origin.getByRole("button", { name: "See the reviews", exact: true }).waitFor();
  await noSidewaysScroll(page, `review lineage ${name}`);
  await analysisTab(page, "Review");
  await noSidewaysScroll(page, `review ${name}`);
  await review.scrollIntoViewIfNeeded();
  await capture(page, `review_${name}`);
  // The project shows each analysis's newest review on its step and among its checks.
  await page.locator("article .running").getByRole("button", { name: "Test diet response study" }).click();
  await projectStep(page, "Summarize each diet and test feature changes").locator(".story-detail").getByText(/Accepted by you$/).waitFor();
  await projectStep(page, "Compare plasma response across the three diets").locator(".story-detail").getByText(/Changes requested by Example colleague$/).waitFor();
  assert.equal(await page.locator("#section-steps").getByText(/Not yet reviewed by a person/).count(), 0, `${name}: every finished analysis has a review`);
  const accepted = await openStep(page, "Summarize each diet and test feature changes");
  await accepted.locator(".step-checks").getByText("Accepted by you", { exact: true }).waitFor();
  // An analysis outside a project has no reviewer and no form.
  await page.goto(home(`analyses/${standaloneTask}`), { waitUntil: "domcontentloaded" });
  await analysisTab(page, "Review");
  await review.getByText("Only an analysis in a project can be reviewed, and this one is not part of a project.", { exact: true }).waitFor();
  assert.equal(await review.locator("form").count(), 0, `${name}: no review form outside a project`);
}

// Accept the result (Glass Box, "Your next step"): a sheet names each finished analysis no person has accepted, the one
// the answer reads first, with its checks, any earlier review and a way to read it first; accepting records the reader's
// own review on each. Once every finished analysis carries a person's accepting review, the stop for review is answered
// and the project reads as finished on its page and in the index. It runs on a fresh fixture, after the other walks.
async function acceptResult(page, name) {
  await reset();
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Test diet response study", level: 1 }).waitFor();
  await page.locator("#section-next").getByRole("button", { name: "Accept the result", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Accept the result" });
  const listed = sheet.getByRole("list", { name: "The analyses you accept" });
  assert.deepEqual(await listed.locator(".accept-title").allTextContents(),
    ["Compare plasma response across the three diets", "Summarize each diet and test feature changes"], `${name}: the analysis the answer reads comes first`);
  const compared = listed.locator("li").filter({ hasText: "Compare plasma response across the three diets" });
  await compared.getByText(/^The answer's findings come from it · /).waitFor();
  await compared.getByText("Changes requested by Example colleague. Your review becomes its current one.", { exact: true }).waitFor();
  await compared.getByText("The agent wrote its code; it stays unvalidated until a person reads it.", { exact: true }).waitFor();
  assert.equal(await listed.getByRole("button", { name: "Read it first", exact: true }).count(), 2, `${name}: each analysis can be read first`);
  await sheet.getByText("It is not statistical validation", { exact: false }).waitFor();
  await noSidewaysScroll(page, `accept ${name}`);
  await capture(page, `accept_${name}`);
  await sheet.getByRole("textbox", { name: "A note with your review", exact: true }).fill(`Read the code and checks in the ${name} pass.`);
  await sheet.getByRole("button", { name: "Accept 2 analyses", exact: true }).click();
  await page.getByText("Your reviews are saved: you accepted 2 analyses.", { exact: true }).first().waitFor();
  for (const title of ["Compare plasma response across the three diets", "Summarize each diet and test feature changes"])
    await projectStep(page, title).locator(".story-detail").getByText(/Accepted by you$/).waitFor();
  const summary = page.locator("#section-summary");
  await summary.getByRole("heading", { name: "Finished", level: 2 }).waitFor();
  await summary.getByText("A person accepted its results. The agent does nothing more until someone tells it to continue.", { exact: true }).waitFor();
  await page.locator("#section-found").getByText("The agent's reading. A person accepted the analyses it rests on.", { exact: true }).waitFor();
  assert.equal(await page.locator("#section-next").getByRole("button", { name: "Accept the result", exact: true }).count(), 0, `${name}: nothing is left to accept`);
  // The index reads the same reviews, so the project is finished there too.
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await projectRow(page, "Test diet response study").getByText("Finished: 2 analyses finished, 1 did not finish. A person accepted each one that finished.", { exact: false }).waitFor();
}

// Lab memory: once the reader accepted the summary, its note can be shared with the lab; the lab's
// shared notes list it with the others, and it can be renewed and retired with a reason.
async function shareNote(page, mobile, name) {
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  const findings = page.locator("#section-findings");
  const note = findings.locator("li").filter({ hasText: "The retained input has no preparation receipt" });
  // The agent's latest decision read one note a colleague shared.
  await findings.getByText("At its latest step the agent also read a note shared in Example Nutrition Lab:", { exact: true }).waitFor();
  await findings.getByText(/Participants reported feeling full for longer/).waitFor();
  // It also read three passages from two papers in the lab's library, each paper listed once with its pages,
  // and a section of a Commons paper, named by its title and linked to the Commons.
  const papers = findings.locator(".read-papers");
  await papers.getByText("At its latest step the agent read passages from 3 papers in the library of Example Nutrition Lab and the Commons:", { exact: true }).waitFor();
  const fibre = papers.getByRole("link", { name: "Fibre-rich breakfasts and reported fullness in a synthetic feeding study", exact: true });
  assert.equal(await fibre.getAttribute("href"), "#/library/papers/80000000-0000-4000-8000-000000000001", `${name}: the paper links to its page`);
  const commons = papers.getByRole("link", { name: "Satiety markers after oat breakfasts: a synthetic commons paper", exact: true });
  assert.equal(await commons.getAttribute("href"), "#/commons", `${name}: a Commons paper links to the Commons`);
  assert.deepEqual(await papers.locator("li").allTextContents().then((all) => all.map((t) => t.replace(/\s+/g, " ").trim())),
    ["Fibre-rich breakfasts and reported fullness in a synthetic feeding study, page 3, page 4",
      "Plasma markers after three test diets: an invented methods note, page 2",
      "Satiety markers after oat breakfasts: a synthetic commons paper, from the Commons, page 6"], `${name}: each paper once, with the pages read`);
  await note.getByRole("button", { name: "Share with the lab", exact: true }).click();
  const form = note.getByRole("form", { name: "Share this note with the lab" });
  await form.getByLabel("Keep it in use for").selectOption({ label: "1 year" });
  await form.getByLabel(/Why share it/).fill(`Checked in the ${name} walk.`);
  await form.getByRole("button", { name: "Share with the lab", exact: true }).click();
  await note.getByText("Shared with the lab: in use", { exact: true }).waitFor();
  assert.equal(await note.getByRole("button", { name: "Share with the lab" }).count(), 0, `${name}: a shared note is not offered again`);
  await noSidewaysScroll(page, `share ${name}`);
  await note.scrollIntoViewIfNeeded();
  await capture(page, `share_${name}`);
  await go(page, mobile, /^Shared notes/);
  await page.getByRole("heading", { name: "Shared notes", level: 1 }).waitFor();
  const inUse = page.locator("ol.notes").first();
  const mine = inUse.locator("li").filter({ hasText: "The retained input has no preparation receipt" });
  await mine.getByText(/^Shared by you on /).waitFor();
  await mine.getByText(`Checked in the ${name} walk.`, { exact: false }).waitFor();
  await inUse.getByText(/Participants reported feeling full for longer/).waitFor();
  await page.getByText("Retired notes (1)", { exact: true }).waitFor();
  await mine.getByRole("button", { name: "Renew", exact: true }).click();
  await mine.getByRole("form", { name: "Renew this note" }).getByLabel("Keep it in use for").selectOption({ label: "2 years" });
  await mine.getByRole("button", { name: "Renew the note", exact: true }).click();
  await mine.getByText(/^Renewed by Example researcher on /).waitFor();
  await noSidewaysScroll(page, `shared notes ${name}`);
  await capture(page, `shared_notes_${name}`);
  await mine.getByRole("button", { name: "Retire", exact: true }).click();
  const retire = mine.getByRole("form", { name: "Retire this note" });
  await retire.getByRole("button", { name: "Retire the note", exact: true }).click();
  await retire.getByText("Say why it should no longer be used.", { exact: true }).waitFor();
  await retire.getByLabel("Why retire it").fill("The summary will be rerun with a preparation record.");
  await retire.getByRole("button", { name: "Retire the note", exact: true }).click();
  await page.getByText("Retired notes (2)", { exact: true }).waitFor();
  // A retired note stays listed with its reason, and its project may share the note again.
  await page.getByText("Retired notes (2)", { exact: true }).click();
  await page.getByText(/: The summary will be rerun with a preparation record\.$/).waitFor();
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  await note.getByRole("button", { name: "Share with the lab", exact: true }).waitFor();
}

async function readLibrary(page, mobile, name) {
  await go(page, mobile, /^Library/);
  await page.getByRole("heading", { name: "Library", level: 1 }).waitFor();
  await page.getByRole("heading", { name: "Papers (2)", level: 2 }).waitFor();
  const papers = page.locator("ol.papers");
  const fibre = papers.locator("li").filter({ hasText: "Fibre-rich breakfasts and reported fullness" });
  await fibre.getByText("Ada Example, Ben Sample, Cora Placeholder and 1 more · 2025", { exact: true }).waitFor();
  // A label only a service account added reads as proposed.
  assert.equal(await fibre.getByTitle(/Proposed by a service account/).count(), 1, `${name}: the proposed label is marked`);
  await fibre.getByText(/^Added by Example colleague on .+ 9 pages · 3 figures found · 42 searchable passages\. Accepted for the research agent by Example researcher on .+\.$/).waitFor();
  // A lead sees how many papers still wait for acceptance and can list only those.
  await page.getByText("1 paper is not accepted for the research agent yet.", { exact: false }).waitFor();
  await page.getByRole("button", { name: "Show only these", exact: true }).click();
  assert.equal(await papers.locator("li").count(), 1, `${name}: only the paper waiting for acceptance is listed`);
  await papers.getByText("Plasma markers after three test diets: an invented methods note", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Show every paper", exact: true }).click();
  assert.equal(await papers.locator("li").count(), 2, `${name}: every paper is listed again`);
  await page.getByLabel("Search the lab's papers").fill("fullness");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const results = page.getByRole("region", { name: "Passages for “fullness”" });
  await results.getByText("Ranked by the words they share with the search, not by meaning.", { exact: true }).waitFor();
  await results.getByText(/Reported fullness stayed higher after the fibre-rich breakfast/).waitFor();
  await results.getByText("Results · page 4", { exact: true }).waitFor();
  await noSidewaysScroll(page, `library ${name}`);
  await capture(page, `library_${name}`);
  await results.getByRole("button", { name: "Clear the search", exact: true }).click();
  // Adding a PDF reads it and opens its page.
  await page.locator("#library-file").setInputFiles({ name: "preview.pdf", mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4\n% synthetic ${name}\n`) });
  await page.getByRole("heading", { name: "A synthetic paper added in the preview", level: 1 }).waitFor();
  await page.getByText("The paper is in the library.", { exact: true }).first().waitFor();
  await page.getByText(/^Journal of Invented Examples\. Added by you on .+ 2 pages · 1 figure found · 3 searchable passages\.$/).waitFor();
  await page.getByRole("heading", { name: "Figures (1)", level: 2 }).waitFor();
  const labels = page.getByRole("form", { name: "Add a label" });
  await labels.getByLabel("Kind").selectOption({ label: "Topic" });
  await labels.getByLabel("Label").fill("breakfast timing");
  await labels.getByRole("button", { name: "Add the label", exact: true }).click();
  await page.locator("ul.labels").getByText("breakfast timing", { exact: true }).waitFor();
  await page.getByText("An invented figure caption.", { exact: true }).waitFor();
  // A lead or reviewer accepts the paper for the research agent, and withdraws that only with a reason.
  const forAgent = page.getByRole("region", { name: "For the research agent" });
  await forAgent.getByText("Not accepted for the research agent yet.", { exact: true }).waitFor();
  await forAgent.getByRole("button", { name: "Accept for the research agent", exact: true }).click();
  await forAgent.getByText(/^Accepted for the research agent by Example researcher on .+\.$/).waitFor();
  await noSidewaysScroll(page, `paper ${name}`);
  await capture(page, `paper_${name}`);
  await forAgent.getByRole("button", { name: "Withdraw the acceptance…", exact: true }).click();
  const withdraw = forAgent.getByRole("form", { name: "Withdraw the acceptance" });
  await withdraw.getByRole("button", { name: "Withdraw the acceptance", exact: true }).click();
  await forAgent.getByText("Say why the acceptance is withdrawn.", { exact: true }).waitFor();
  await withdraw.getByLabel("Why withdraw it").fill("Accepted only to check the preview.");
  await withdraw.getByRole("button", { name: "Withdraw the acceptance", exact: true }).click();
  await forAgent.getByText("Not accepted for the research agent yet.", { exact: true }).waitFor();
  await forAgent.getByRole("button", { name: "Accept for the research agent", exact: true }).click();
  await forAgent.getByText(/^Accepted for the research agent by Example researcher on .+\.$/).waitFor();
  await page.getByRole("button", { name: "Remove this paper…", exact: true }).click();
  const remove = page.getByRole("form", { name: "Remove this paper" });
  await remove.getByRole("button", { name: "Remove the paper", exact: true }).click();
  await page.getByText("Say why it should be removed.", { exact: true }).waitFor();
  await remove.getByLabel("Why remove it").fill("Added only to check the preview.");
  await remove.getByRole("button", { name: "Remove the paper", exact: true }).click();
  await page.getByText(/^Removed from the library on .+: Added only to check the preview\. Adding the same file again restores it\.$/).waitFor();
  assert.equal(await forAgent.count(), 0, `${name}: a removed paper is not offered for acceptance`);
  await page.getByRole("link", { name: "Library", exact: true }).first().click();
  await page.getByText("One paper was removed from the library.", { exact: true }).waitFor();
  assert.equal(await papers.getByText("A synthetic paper added in the preview").count(), 0, `${name}: a removed paper leaves the list`);
  // A person sets the proposed label.
  await fibre.getByRole("link", { name: /Fibre-rich breakfasts/ }).click();
  const proposed = page.locator("ul.labels li").filter({ hasText: "crossover trial" });
  await proposed.getByText("Proposed by a service account.", { exact: true }).waitFor();
  await proposed.getByRole("button", { name: "Set it", exact: true }).click();
  await proposed.getByText(/^Set on /).waitFor();
  // Several PDFs in one step: read one at a time, each listed with where it stands, a file that is not a PDF named and
  // left out, and a file whose answer the preview drops settled from the reloaded list by its bytes; then a lead
  // accepts every waiting paper after one confirming step, the dropped acceptance settled the same way.
  await page.getByRole("link", { name: "Library", exact: true }).first().click();
  await page.getByRole("heading", { name: "Papers (2)", level: 2 }).waitFor();
  await page.locator("#library-file").setInputFiles([
    { name: "batch-one.pdf", mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4\n% batch one ${name}\n`) },
    { name: "batch-two.pdf", mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4\n% batch two ${name}\n`) },
    { name: "batch-lost.pdf", mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4\n% lose-answer ${name}\n`) },
    { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("not a paper") },
  ]);
  const batch = page.getByRole("region", { name: "Added to Example Nutrition Lab from 4 files" });
  await batch.waitFor();
  await batch.getByText("3 papers added; 1 file was not added.", { exact: true }).waitFor();
  await batch.locator("li").filter({ hasText: "notes.txt" }).getByText("Choose a PDF file.", { exact: true }).waitFor();
  await batch.locator("li").filter({ hasText: "batch-lost.pdf" }).getByText("Added.", { exact: true }).waitFor();
  assert.equal(await batch.locator("li").filter({ hasText: "Added." }).count(), 3, `${name}: three PDFs were added, one settled after its answer was lost`);
  await page.getByRole("heading", { name: "Papers (5)", level: 2 }).waitFor();
  assert.equal(await page.getByRole("heading", { name: "A synthetic paper added in the preview", level: 1 }).count(), 0, `${name}: a batch stays on the library`);
  await page.getByText("4 papers are not accepted for the research agent yet.", { exact: false }).waitFor();
  await noSidewaysScroll(page, `library batch ${name}`);
  await capture(page, `library_batch_${name}`);
  const acceptAllButton = page.getByRole("button", { name: "Accept all 4…", exact: true });
  await acceptAllButton.click();
  const confirm = page.getByRole("group", { name: /^Accept the 4 waiting papers for the research agent\?/ });
  const confirmButton = confirm.getByRole("button", { name: "Accept 4 papers", exact: true });
  await page.waitForFunction(() => document.activeElement?.textContent?.trim() === "Accept 4 papers", null, { timeout: 5000 });
  await confirm.getByRole("button", { name: "Cancel", exact: true }).click();
  assert.equal(await confirm.count(), 0, `${name}: cancelling closes the confirming step`);
  await page.waitForFunction(() => document.activeElement?.textContent?.trim() === "Accept all 4…", null, { timeout: 5000 });
  await acceptAllButton.click();
  await confirmButton.click();
  await page.getByText("4 papers accepted for the research agent.", { exact: true }).first().waitFor();
  assert.equal(await page.getByText(/papers? (is|are) not accepted for the research agent yet\./).count(), 0, `${name}: nothing waits after accepting all`);
  // A single paper after a batch leaves the batch's list behind.
  await page.locator("#library-file").setInputFiles({ name: "after-batch.pdf", mimeType: "application/pdf", buffer: Buffer.from(`%PDF-1.4\n% after batch ${name}\n`) });
  await page.getByText("The paper is in the library.", { exact: true }).first().waitFor();
  await page.getByRole("link", { name: "Library", exact: true }).first().click();
  await page.getByRole("heading", { name: "Papers (6)", level: 2 }).waitFor();
  assert.equal(await batch.count(), 0, `${name}: a single upload clears the batch's list`);
  // One waiting paper is offered the same confirmed step.
  await page.getByText("1 paper is not accepted for the research agent yet.", { exact: false }).waitFor();
  await page.getByRole("button", { name: "Accept it…", exact: true }).click();
  await page.getByRole("group", { name: /^Accept the waiting paper for the research agent\?/ }).getByRole("button", { name: "Accept 1 paper", exact: true }).click();
  await page.getByText("1 paper accepted for the research agent.", { exact: true }).first().waitFor();
}

// The commons: a person who leads it marks a record baseline for every lab and withdraws the mark,
// with the unmarked record carrying the editor's change bar; a paper's mark covers its parts and names the newer ones;
// a whole collection is marked after one confirming step; a change whose answer is lost reloads what the service holds;
// anyone else reads the lists with no control and a reason.
async function readCommons(page, mobile, name) {
  await go(page, mobile, /^Commons/);
  await page.getByRole("heading", { name: "Commons", level: 1 }).waitFor();
  await page.getByText("4 datasets, 1 marked baseline. 2 records are not marked. Mark those every lab should read.", { exact: true }).waitFor();
  const records = page.locator("ol.records:not(.collection-list)");
  const portions = records.locator("li").filter({ hasText: "Invented portion sizes for common foods" });
  assert.equal(await records.locator("li.waits").count(), 2, `${name}: only the unmarked active records carry the change bar`);
  const codebook = records.locator("li").filter({ hasText: "Placeholder dietary survey codebook" });
  await codebook.getByText("Public reference data · rights public · superseded · added", { exact: false }).waitFor();
  assert.equal(await codebook.getByRole("button").count(), 0, `${name}: a superseded record is not offered the mark`);
  await portions.getByRole("button", { name: "Mark baseline", exact: true }).click();
  await page.getByText("Marked baseline: every lab's research agent now reads “Invented portion sizes for common foods”.", { exact: true }).first().waitFor();
  await portions.getByText("Every lab reads it", { exact: true }).waitFor();
  await page.getByText("4 datasets, 2 marked baseline.", { exact: false }).waitFor();
  assert.equal(await records.locator("li.waits").count(), 1, `${name}: the change bar draws back once the mark is made`);
  await noSidewaysScroll(page, `commons ${name}`);
  await capture(page, `commons_${name}`);
  await portions.getByRole("button", { name: "Withdraw the mark", exact: true }).click();
  await page.getByText("Mark withdrawn: “Invented portion sizes for common foods” is read only through the commons now.", { exact: true }).first().waitFor();
  await portions.getByText("Commons only", { exact: true }).waitFor();
  // Papers come a page at a time; a marked paper names the parts added since its mark and covers them when marked again.
  await page.getByRole("button", { name: "Papers", exact: true }).click();
  await page.getByText("The newest 2 papers shown, 1 marked baseline; more follow.", { exact: false }).waitFor();
  await page.getByRole("button", { name: "Show more papers", exact: true }).click();
  await page.getByText("3 papers, 1 marked baseline.", { exact: false }).waitFor();
  const trial = records.locator("li").filter({ hasText: "A made-up feeding trial with three test diets" });
  await trial.getByText("2 sections, figures or tables were added since the mark and are read through the commons only.", { exact: true }).waitFor();
  await trial.getByRole("button", { name: "Mark the newer parts", exact: true }).click();
  await page.getByText("Marked baseline with 2 sections, figures and tables: every lab's research agent now reads “A made-up feeding trial with three test diets”.", { exact: true }).first().waitFor();
  assert.equal(await trial.getByText(/added since the mark/).count(), 0, `${name}: no part waits once the paper is marked again`);
  // A collection: its records by kind, what every lab reads, and one mark for all of it after a confirming step.
  const collection = page.locator("ol.collection-list li").filter({ hasText: "SYNTH literature" });
  await collection.getByText("3 papers, 1 dataset.", { exact: true }).waitFor();
  await collection.getByText("Every lab reads 1 of its 4 records.", { exact: true }).waitFor();
  await collection.getByText("Partly marked", { exact: true }).waitFor();
  await collection.getByRole("button", { name: "Mark the rest of the collection", exact: true }).click();
  await collection.getByText(/^Marking lets every lab's research agent read all 4 records of this collection/).waitFor();
  await noSidewaysScroll(page, `commons collection ${name}`);
  await capture(page, `commons_collection_${name}`);
  await collection.getByRole("button", { name: "Mark the collection baseline", exact: true }).click();
  await page.getByText("Marked 3 records and 10 sections, figures and tables of SYNTH literature baseline, in your name. Every lab's research agent now reads them.", { exact: true }).first().waitFor();
  await collection.getByText("Every lab's research agent reads all of it.", { exact: true }).waitFor();
  await collection.getByText("Every lab reads it", { exact: true }).waitFor();
  // A marked paper gains parts: the fully marked collection offers to mark the newer parts, and the confirmation it
  // opens marks them. It must never withdraw the collection.
  const added = await fetch(new URL("/api/v1/fixture/knowledge-parts-added", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(added.status, 200, "the preview adds parts to a marked paper");
  await page.getByRole("button", { name: "Datasets", exact: true }).click();
  await page.getByRole("button", { name: "Papers", exact: true }).click();
  await collection.getByRole("button", { name: "Mark the newer parts", exact: true }).click();
  await collection.getByText(/^Marking lets every lab's research agent read all 4 records of this collection/).waitFor();
  await collection.getByRole("button", { name: "Mark the newer parts", exact: true }).click();
  await page.getByText(/of SYNTH literature baseline, in your name/).first().waitFor();
  await collection.getByText("Every lab reads it", { exact: true }).waitFor();
  // The collection list reloads after the mark; once it has, no part waits.
  await collection.getByRole("button", { name: "Mark the newer parts", exact: true }).waitFor({ state: "detached" });
  // The answer to the next change is lost: the page says the outcome is unknown and shows what the service holds.
  const drop = await fetch(new URL("/api/v1/fixture/knowledge-drop-next", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(drop.status, 200, "the preview drops the next answer");
  await collection.getByRole("button", { name: "Withdraw the collection's mark", exact: true }).click();
  await collection.getByRole("button", { name: "Withdraw the collection's mark", exact: true }).click();
  await collection.getByText("The knowledge service did not confirm the change, so it may or may not have been made. The list was reloaded to show what the service holds now.", { exact: true }).waitFor();
  await collection.getByText("Commons only", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Tools", exact: true }).click();
  await page.getByText("One tool, marked baseline.", { exact: false }).waitFor();
  await page.getByRole("button", { name: "Object collections", exact: true }).click();
  await page.getByText("The commons holds no object collection yet.", { exact: false }).waitFor();
  // A member reads the same list without a control, and is told why.
  const member = await fetch(new URL("/api/v1/fixture/commons-role?role=member", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(member.status, 200, "the preview takes the commons role");
  await page.getByRole("button", { name: "Datasets", exact: true }).click();
  await page.getByText("You are a member of the commons. Only a person who leads the commons can change what every lab receives.", { exact: false }).waitFor();
  assert.equal(await records.getByRole("button", { name: /Mark baseline|Withdraw the mark/ }).count(), 0, `${name}: no control is offered to a member`);
  assert.equal(await records.locator("li.waits").count(), 0, `${name}: nothing waits for a member`);
  await noSidewaysScroll(page, `commons member ${name}`);
  await capture(page, `commons_member_${name}`);
}

// Links the knowledge service proposed from the lab's papers: a member reads the queue with no
// control; a curator accepts one and rejects another with a note, each leaving the queue.
async function reviewLinks(page, mobile, name) {
  const labRole = async (role) => {
    const response = await fetch(new URL(`/api/v1/fixture/lab-role?role=${role}`, target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
    assert.equal(response.status, 200, "the preview takes the lab role");
  };
  await labRole("member");
  await go(page, mobile, /^Library/);
  const section = page.getByRole("region", { name: /^Proposed links/ });
  await section.getByText("2 proposed links wait for a person who curates or leads the lab to review them.", { exact: true }).waitFor();
  assert.equal(await section.getByRole("button").count(), 0, `${name}: no control is offered to a member`);
  assert.equal(await section.locator("li.waits").count(), 0, `${name}: nothing waits for a member`);
  await labRole("curator");
  await page.reload({ waitUntil: "domcontentloaded" });
  await section.getByRole("heading", { name: "Proposed links (2)", level: 2 }).waitFor();
  await section.getByText("2 proposed links wait for your review.", { exact: true }).waitFor();
  assert.equal(await section.locator("li.waits").count(), 2, `${name}: each waiting link carries the change bar`);
  const first = section.locator("li").filter({ hasText: "Public R mean bootstrap" });
  await first.getByText("Proposed because “bootstrap” appears in 2 sections.", { exact: true }).waitFor();
  await noSidewaysScroll(page, `links ${name}`);
  await capture(page, `links_${name}`);
  await first.getByRole("button", { name: "Accept the link", exact: true }).click();
  await page.getByText("Link accepted and recorded in your name.", { exact: true }).first().waitFor();
  await section.getByRole("heading", { name: "Proposed links (1)", level: 2 }).waitFor();
  const second = section.locator("li").filter({ hasText: "Public R median bootstrap" });
  await second.getByRole("button", { name: "Reject…", exact: true }).click();
  const reject = second.getByRole("form", { name: "Reject this link" });
  await reject.getByLabel("Why reject it (optional)").fill("The note describes sample handling, not this tool.");
  // The rejection's answer is lost after the service made it: the page says so, reads the queue again and shows it
  // empty, instead of leaving the link listed.
  const dropReview = await fetch(new URL("/api/v1/fixture/knowledge-drop-next", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(dropReview.status, 200, "the preview drops the next answer");
  await reject.getByRole("button", { name: "Reject the link", exact: true }).click();
  await page.getByText("The knowledge service did not confirm the change, so it may or may not have been made. The list was reloaded to show what the service holds now.", { exact: true }).first().waitFor();
  await page.getByRole("region", { name: /^Proposed links/ }).waitFor({ state: "detached" });
}

async function openRecord(page) {
  const record = page.locator("#section-record");
  if (!(await record.evaluate((d) => d.open))) await record.locator("summary").first().click();
  return page.locator("#section-notebook");
}

async function editQuestionAndRun(page, name) {
  const question = `Which diet changes plasma response most in the ${name} run?`;
  await page.getByRole("button", { name: "Edit the question", exact: true }).click();
  const edit = page.getByRole("dialog", { name: "Edit the question" });
  await edit.getByLabel("Research question", { exact: true }).fill(question);
  await edit.getByRole("button", { name: "Save a new version", exact: true }).click();
  await page.getByText(question, { exact: false }).first().waitFor();
  const notebook = await openRecord(page);
  await notebook.getByText("Research question edited").first().waitFor();
  // Asking for changes opens the composer in place; sending saves the instruction with the question and starts the
  // agent again with the same methods.
  const next = page.locator("#section-next");
  await next.getByRole("button", { name: "Ask for changes", exact: true }).click();
  const instruction = `Also compare the two test diets directly in the ${name} check.`;
  await page.getByLabel("Tell the agent what to change or do next").fill(instruction);
  await page.getByRole("button", { name: "Send and start again", exact: true }).click();
  await page.getByText("Your instruction is saved with the question, and the agent has started again.").first().waitFor();
  await page.locator("#section-summary").getByRole("heading", { name: "Working now", level: 2 }).waitFor();
  await next.getByRole("heading", { name: "Instructions to the agent" }).waitFor();
  await next.getByText(instruction, { exact: true }).waitFor();
  const given = next.locator(".instructions li").filter({ hasText: instruction });
  await given.getByText("Not read yet: the agent has not decided anything since.", { exact: true }).waitFor();
  await notebook.getByText("Instruction to the agent").first().waitFor();
  // While it works the status says what the system does and for how long, and offers Pause and Tell the agent.
  const summary = page.locator("#section-summary");
  await summary.getByText("Choosing the next step", { exact: true }).waitFor();
  await summary.getByRole("button", { name: "Tell the agent", exact: true }).waitFor();
  await summary.getByRole("button", { name: "Pause the agent", exact: true }).click();
  await page.getByText("The agent is paused.", { exact: false }).first().waitFor();
  await summary.getByRole("heading", { name: "Paused", level: 2 }).waitFor();
  await summary.getByRole("button", { name: "Resume", exact: true }).waitFor();
  await page.getByRole("button", { name: "Continue with different methods", exact: true }).click();
  const start = page.getByRole("dialog", { name: "Continue the research" });
  assert.equal(await start.getByRole("checkbox", { name: /Evidence review/ }).isChecked(), true, "the last session's methods are the default");
  await start.getByRole("button", { name: "Start from the saved plan", exact: true }).click();
  await summary.getByRole("button", { name: "Pause the agent", exact: true }).click();
  await page.getByRole("button", { name: "Continue with different methods", exact: true }).waitFor();
  // Resume starts again with the same methods and limits in one step.
  await summary.getByRole("button", { name: "Resume", exact: true }).click();
  await page.getByText("The agent started again with the same methods and limits. You can leave this page.").first().waitFor();
  await summary.getByRole("heading", { name: "Working now", level: 2 }).waitFor();
  await summary.getByRole("button", { name: "Pause the agent", exact: true }).click();
  await summary.getByRole("heading", { name: "Paused", level: 2 }).waitFor();
  // Withdrawing asks in the page first, and the instruction stays listed as withdrawn.
  await given.getByRole("button", { name: "Withdraw", exact: true }).click();
  const confirm = given.getByRole("group", { name: "Withdraw this instruction" });
  await confirm.getByRole("button", { name: "Keep it", exact: true }).click();
  await confirm.waitFor({ state: "hidden" });
  await given.getByRole("button", { name: "Withdraw", exact: true }).click();
  await confirm.getByRole("button", { name: "Withdraw", exact: true }).click();
  await page.getByText("The instruction is withdrawn. The agent stops following it at its next decision.").first().waitFor();
  await given.getByText(/^Withdrawn by you, .+, before the agent read it\.$/).waitFor();
  assert.equal(await given.getByRole("button", { name: "Withdraw", exact: true }).count(), 0, `${name}: a withdrawn instruction offers no second withdrawal`);
}

async function shareAccess(page) {
  // Sharing is one line on the page; Manage opens the people and their access.
  await page.locator(".share-line").getByRole("button", { name: "Manage", exact: true }).click();
  const manage = page.getByRole("dialog", { name: "People and access" });
  await manage.getByRole("button", { name: "Invite a colleague", exact: true }).click();
  const invite = page.getByRole("dialog", { name: "Invite a colleague" });
  await invite.getByLabel("Colleague").selectOption({ label: "Second colleague · second@example.invalid" });
  await invite.getByRole("radio", { name: "Can review" }).check();
  await invite.getByRole("button", { name: "Give access", exact: true }).click();
  const people = page.locator("#section-people");
  await people.getByText("second@example.invalid").first().waitFor();
  await people.locator("li", { hasText: "second@example.invalid" }).getByRole("button", { name: "Remove", exact: true }).click();
  await page.getByRole("dialog", { name: "Remove access" }).getByRole("button", { name: "Remove access", exact: true }).click();
  await people.getByText("Second colleague: access removed").waitFor();
  await manage.getByRole("button", { name: "Close" }).click();
  await manage.waitFor({ state: "hidden" });
}

async function downloadPackage(page) {
  await page.locator("#section-next").getByRole("button", { name: "Download everything", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Evidence package" });
  await dialog.getByText("Saved versions").waitFor();
  // The counts keep their height when the dialog's content is taller than the dialog and scrolls.
  const counts = dialog.locator("dl.contents");
  assert.ok((await counts.evaluate((e) => e.scrollHeight - e.clientHeight)) <= 1, "the counts are not clipped");
  // The rerun kit is named, with how many analyses it can check.
  await dialog.getByText("Its rerun kit checks every file", { exact: false }).waitFor();
  await dialog.getByText("Analyses the rerun kit can check", { exact: true }).waitFor();
  // The SCINet steps open from the dialog, Atlas first, and the switch shows each system's own steps.
  const hpc = dialog.locator("details.hpc");
  await hpc.locator("summary").click();
  assert.equal(await hpc.getByRole("button", { name: "Atlas", exact: true }).getAttribute("aria-pressed"), "true");
  assert.equal(await hpc.locator("ol.steps > li").count(), 3, "every step is listed");
  await hpc.getByText("sbatch -A YOUR_PROJECT rerun/slurm/rerun.sbatch", { exact: true }).waitFor();
  await hpc.getByRole("button", { name: "Ceres", exact: true }).click();
  assert.equal(await hpc.getByRole("link", { name: "Go to Ceres Open OnDemand" }).getAttribute("href"), "https://ceres-ondemand.scinet.usda.gov/");
  await hpc.getByText("Sign in to Ceres Open OnDemand", { exact: false }).waitFor();
  await hpc.locator("summary").click();
  const [download] = await Promise.all([page.waitForEvent("download"), dialog.getByRole("button", { name: "Download the package", exact: true }).click()]);
  assert.equal(download.suggestedFilename(), "research-evidence-test-diet-response-study.tar.gz");
  const bytes = await readFile(await download.path());
  assert.ok(bytes.length > 100 && bytes[0] === 0x1f && bytes[1] === 0x8b, "the saved package is the gzip the server sent");
  await download.delete();
  await page.getByText("The browser checked it against the server's SHA-256", { exact: false }).first().waitFor();
  await dialog.getByRole("button", { name: "Done", exact: true }).click();
}

async function readAnalysis(page, mobile, name) {
  await page.goto(home(`projects/${main}/analyses/${firstTask}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Summarize each diet and test feature changes", level: 1 }).waitFor();
  await everyTabFits(page, "Sections of this analysis", `analysis ${name}`);
  await analysisTab(page, "How it was made");
  await page.getByText("download the analysis package from the Files tab", { exact: false }).waitFor();
  // The analysis says until when its working copy is kept, and that the results and package stay.
  await analysisTab(page, "Files");
  const copy = page.locator("section[aria-labelledby='a-files'] p.working-copy");
  assert.match(await copy.innerText(), /^Working copy kept until [A-Z][a-z]+ \d{1,2}(, \d{4})? \(in 30 days\)\. Results and the package stay\.$/, `${name}: the working copy line`);
  const files = page.getByRole("list", { name: "Retained files" });
  // Each file says in plain words what it is for; its path stays in the file record.
  const summaryFile = files.getByRole("button", { name: /^Condition summary A results table · / });
  assert.doesNotMatch(await summaryFile.innerText(), /results\/condition_summary\.csv/, `${name}: the list names the file's use, not its path`);
  await summaryFile.click();
  await page.locator(".file-reader").getByRole("columnheader", { name: "standard_deviation" }).waitFor();

  const index = page.locator(".step-index");
  const detail = page.locator(".step-inspector");
  await index.getByRole("button", { name: /Step 2 of 100: Recorded tool step/ }).click();
  await detail.locator("table").waitFor({ state: "visible", timeout: 10_000 });
  const table = await detail.locator("table").boundingBox();
  const height = page.viewportSize().height;
  assert.ok(table && table.y >= 0 && table.y < height, "a retained original table must open in the selected step");
  await detail.getByText("These retained files were recorded after this tool step.", { exact: false }).waitFor();
  const [download] = await Promise.all([page.waitForEvent("download"), detail.getByRole("button", { name: "Download", exact: true }).first().click()]);
  await download.delete();
  if (mobile) {
    await detail.getByRole("button", { name: "Back to steps", exact: true }).click();
    await index.waitFor({ state: "visible" });
  }
  const selected = index.locator(".step-results button.selected");
  assert.ok(await contrast(selected.locator("strong")) >= 4.5, "the selected step must keep readable contrast");
  await index.getByRole("combobox", { name: "Show", exact: true }).selectOption("failed");
  if (mobile) await index.getByRole("button", { name: /Recorded tool step that did not finish/ }).first().click();
  await detail.getByText("Recorded tool step that did not finish", { exact: true }).waitFor();
  if (mobile) await detail.getByRole("button", { name: "Back to steps", exact: true }).click();
  await index.getByRole("textbox", { name: "Find a step" }).fill("no_matching_step");
  assert.equal(await index.locator(".step-results button").count(), 0);
  assert.equal(await detail.locator("table").count(), 0, "an empty filter must not show another step's data");
  await index.getByRole("textbox", { name: "Find a step" }).fill("");
  await index.getByRole("combobox", { name: "Show", exact: true }).selectOption("");
  const shownDate = (await index.locator(".step-results small").first().innerText()).split(",")[0];
  await index.getByRole("textbox", { name: "Find a step" }).fill(shownDate);
  assert.ok(await index.locator(".step-results button").count() > 0, "displayed dates remain searchable");
  await index.getByRole("textbox", { name: "Find a step" }).fill("");
  await index.getByRole("button", { name: "Load more steps" }).click();
  await index.getByText("200 recorded tool steps loaded", { exact: true }).waitFor();
  await index.getByRole("button", { name: "Load more steps" }).click();
  await index.getByText("240 recorded tool steps loaded", { exact: true }).waitFor();
  await index.getByRole("spinbutton", { name: "Go to step" }).fill("200");
  await index.getByRole("button", { name: "Go", exact: true }).click();
  await detail.getByText("Step 200 of 240 loaded", { exact: true }).waitFor();
  await detail.getByRole("button", { name: "Next", exact: true }).click();
  await detail.getByText("Step 201 of 240 loaded", { exact: true }).waitFor();
  if (mobile) await detail.getByRole("button", { name: "Back to steps", exact: true }).click();
  await index.getByRole("spinbutton", { name: "Go to step" }).fill("240");
  await index.getByRole("button", { name: "Go", exact: true }).click();
  await detail.getByText("Step 240 of 240 loaded", { exact: true }).waitFor();
  await noSidewaysScroll(page, `analysis ${name}`);
  await capture(page, `analysis_${name}`);
  await go(page, mobile, "Results");
  await page.getByRole("heading", { name: "Results", level: 1 }).waitFor();
  await page.getByText("Summarize plasma response by diet and test feature changes").first().waitFor();
  // All results reads like a portal's jobs table: filtered by where each analysis stands and sorted by its column heads.
  const jobs = page.getByRole("table");
  await page.getByRole("group", { name: "Show analyses" }).getByRole("button", { name: /^Stopped for review/ }).click();
  await jobs.getByRole("link", { name: "Review feature changes before choosing follow-up measures" }).waitFor();
  assert.equal(await jobs.getByRole("link", { name: /Summarize plasma response/ }).count(), 0, `${name}: the status filter narrows the table`);
  await page.getByRole("group", { name: "Show analyses" }).getByRole("button", { name: /^All/ }).click();
  if (!mobile) {
    await jobs.getByRole("button", { name: "Status" }).click();
    assert.equal(await jobs.locator("thead th").first().getAttribute("aria-sort"), "ascending", `${name}: the status head sorts the table`);
  }
  // Results is a place in the sidebar, which marks it; only a page inside a place carries crumbs.
  assert.equal(await page.getByRole("navigation", { name: "Breadcrumb" }).count(), 0, `${name}: a sidebar place carries no crumbs`);
  if (!mobile) assert.equal(await page.getByRole("navigation", { name: "Places" }).getByRole("link", { name: "Results", exact: true }).getAttribute("aria-current"), "page",
    `${name}: the sidebar marks Results`);
}

async function startBlankProject(page, name) {
  await page.goto(home(`projects/${blank}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "What would you like to find out?" }).waitFor();
  const question = `Which features should the follow-up measure after the ${name} check?`;
  // The lab registers three sets; this project starts from one of them.
  const save = page.getByRole("button", { name: "Save the question", exact: true });
  await page.getByLabel("Research question", { exact: true }).fill(question);
  assert.ok(await save.isDisabled(), `${name}: a project's setup waits for a set`);
  await page.getByRole("checkbox", { name: /^Simulated diet study with the reviewed summary method/ }).check();
  await page.getByText("The agent works from Simulated diet study with the reviewed summary method.", { exact: true }).waitFor();
  await page.getByLabel(/Study constraints/).fill("Treat each participant as the experimental unit.");
  await noSidewaysScroll(page, `setup ${name}`);
  await save.click();
  await page.getByText(question, { exact: false }).first().waitFor();
  await page.getByRole("button", { name: "Start the research", exact: true }).waitFor();
}

// While a project opens, its loading line sits where the page will be, not at the window's edge.
async function loadingInColumn(page, name) {
  const projectRead = `**/api/v1/projects/${main}`;
  await page.route(projectRead, async (route) => { await new Promise((done) => setTimeout(done, 1500)); await route.continue().catch(() => {}); });
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  const opening = await page.locator(".page-state[role='status']").boundingBox();
  await page.locator("article.page").waitFor();
  const opened = await page.locator("article.page").boundingBox();
  await page.unroute(projectRead);
  assert.ok(opening && opened && Math.abs(opening.x - opened.x) < 1 && Math.abs(opening.width - opened.width) < 1,
    `${name}: the loading line shares the page's column`);
}

// Two navigations in one task skip the first view transition; that only drops its animation and reports no error.
async function skippedTransition(page) {
  await page.goto(home(""), { waitUntil: "domcontentloaded" });
  await page.locator("nav#sidebar a[href='#/analyses']").waitFor({ state: "attached" });
  await page.evaluate(() => {
    document.querySelector("nav#sidebar a[href='#/analyses']").click();
    document.querySelector("nav#sidebar a[href='#/waiting']").click();
  });
  await page.waitForFunction(() => location.hash === "#/waiting");
  await page.waitForTimeout(300);
}

async function displaySettings(page) {
  await page.getByRole("button", { name: "Display", exact: true }).click();
  await page.getByText("Larger", { exact: true }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.typeSize), "large");
  await page.getByText("Standard", { exact: true }).click();
  assert.equal(await page.evaluate(() => document.documentElement.dataset.typeSize ?? "standard"), "standard");
  await page.keyboard.press("Escape");
}

async function inspect(page, mobile, name) {
  // Inspection is an administrator's place in the account menu, at every width.
  await page.locator("header.bar").getByRole("button", { name: /^Account: signed in as/ }).click();
  await page.locator("#account-menu").getByRole("link", { name: /^Inspection/ }).click();
  await page.getByRole("heading", { name: "Project inspection", level: 1 }).waitFor();
  await page.getByRole("row", { name: /A colleague's private project/ }).getByRole("button", { name: "Inspect", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Start an inspection" });
  await dialog.getByLabel(/Reason/).fill("Quality review of retained evidence");
  await dialog.getByRole("button", { name: "Start the inspection", exact: true }).click();
  await page.getByRole("heading", { name: "A colleague's private project", level: 1 }).waitFor();
  await page.getByText("Administrator inspection.", { exact: false }).waitFor();
  // The server's project view carries the inspection, so a reload keeps the notice.
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByText("Administrator inspection.", { exact: false }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Edit the question" }).count(), 0, "an inspection is read-only");
  await capture(page, `inspection_${name}`);
  await page.getByRole("button", { name: "End inspection", exact: true }).click();
  await page.getByRole("heading", { name: "Project inspection", level: 1 }).waitFor();
  await page.getByText("Inspection ended.", { exact: false }).first().waitFor();
  await page.goto(home(`projects/${foreign}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Project unavailable", level: 1 }).waitFor();
}

// A long run (start it and walk away). The project page follows its project through the project
// stream, so a change made elsewhere shows without a reload and without a five-second poll. While the tab is hidden the
// page waits to read, the tab title still says when work stops and how much waits, and the opt-in notice names no
// project. The page's visibility and the notice are stand-ins; the stream, the fixture and the page are not.
async function followAProject(page, context, name) {
  const sensors = "50000000-0000-4000-8000-000000000006";
  await page.addInitScript(() => {
    const Real = window.Notification;
    window.__notices = [];
    window.__hidden = false;
    Object.defineProperty(Document.prototype, "hidden", { configurable: true, get: () => window.__hidden });
    Object.defineProperty(Document.prototype, "visibilityState", { configurable: true, get: () => (window.__hidden ? "hidden" : "visible") });
    window.Notification = class {
      static get permission() { return Real.permission; }
      static requestPermission() { return Real.requestPermission(); }
      constructor(title, options) { window.__notices.push({ title, body: options?.body ?? "" }); }
      close() {}
    };
  });
  await page.goto(home(`projects/${sensors}`), { waitUntil: "domcontentloaded" });
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Soil moisture sensor comparison", level: 1 }).waitFor();
  const settings = page.locator("#display-settings");
  // Without the browser's permission the setting stays off and the choice shown returns to Off.
  await page.getByRole("button", { name: "Display", exact: true }).click();
  await settings.getByText("On", { exact: true }).click();
  await page.waitForFunction(() => document.querySelector('input[name="notify"][value="off"]')?.checked === true);
  assert.equal(await page.evaluate(() => localStorage.getItem("workspace_notify")), null);
  // A headless browser refuses the request, which is the browser blocking it.
  await settings.getByText("This browser blocks notifications from the workspace. Allow them in the browser's site settings to turn this on.", { exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await context.grantPermissions(["notifications"], { origin: target.origin });
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Soil moisture sensor comparison", level: 1 }).waitFor();
  await page.locator("#section-summary").getByText("The research agent is choosing the next step.", { exact: true }).waitFor();
  await page.waitForFunction(() => /^(\(\d+\) )?Working · Soil moisture sensor comparison · /.test(document.title));
  // Opt in where the display settings are; the choice is kept in this browser.
  await page.getByRole("button", { name: "Display", exact: true }).click();
  await settings.getByText("Only while this tab is in the background. A notice names no project.", { exact: true }).waitFor();
  await settings.getByText("On", { exact: true }).click();
  await page.waitForFunction(() => localStorage.getItem("workspace_notify") === "on");
  await page.keyboard.press("Escape");
  if (name === "desktop_light") {
    // Live, the page reads the project only when it changes: six quiet seconds bring no read, where the page used to
    // read every five seconds while work ran.
    let reads = 0;
    const count = (request) => { if (new URL(request.url()).pathname === `/api/v1/projects/${sensors}`) reads++; };
    page.on("request", count);
    await page.waitForTimeout(6000);
    page.off("request", count);
    assert.equal(reads, 0, "a followed page does not poll");
  }
  // Steering while it works: the instruction is saved with the question and the running session reads it at its next
  // decision; nothing starts a second session.
  const starts = [];
  const countStarts = (request) => { if (request.method() === "POST" && new URL(request.url()).pathname === `/api/v1/projects/${sensors}/runs`) starts.push(request.url()); };
  page.on("request", countStarts);
  const steer = "Compare the two sensors plot by plot before pooling them.";
  // Tell the agent in the status opens the composer in place and moves the keyboard to it.
  await page.locator("#section-summary").getByRole("button", { name: "Tell the agent", exact: true }).click();
  await page.waitForFunction(() => document.activeElement?.id === "ask-text");
  await page.getByLabel("Tell the agent what to change or do next").fill(steer);
  await page.getByText("The agent reads it at its next decision; analyses already running are not stopped.", { exact: false }).waitFor();
  await page.getByRole("button", { name: "Send to the agent", exact: true }).click();
  await page.getByText("Your instruction is saved. The agent reads it at its next decision.", { exact: true }).waitFor();
  await page.locator(".instructions").getByText(steer, { exact: true }).waitFor();
  await page.locator("#section-summary").getByText("The research agent is choosing the next step.", { exact: true }).waitFor();
  page.off("request", countStarts);
  assert.deepEqual(starts, [], "an instruction to a working agent starts no second session");
  const waitingBefore = Number((await page.evaluate(() => document.title)).match(/^\((\d+)\) /)?.[1] ?? 0);
  await page.evaluate(() => { window.__hidden = true; document.dispatchEvent(new Event("visibilitychange")); });
  const stopped = await fetch(new URL("/api/v1/fixture/sensor-run-stops", target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
  assert.equal(stopped.status, 200);
  // Hidden, the tab learns that work stopped and that one more request waits, and the notice says so without the
  // project's name; the page itself reads again only when it is shown.
  await page.waitForFunction((n) => document.title.startsWith(`(${n}) Soil moisture sensor comparison · `), waitingBefore + 1);
  await page.waitForFunction(() => window.__notices.length > 0);
  assert.deepEqual(await page.evaluate(() => window.__notices),
    [{ title: "Waiting for you", body: "A research agent stopped and asks you to review before it continues." }]);
  assert.equal(await page.locator("#section-summary").getByText("The research agent is choosing the next step.", { exact: true }).count(), 1, "a hidden page waits to read");
  await page.evaluate(() => { window.__hidden = false; document.dispatchEvent(new Event("visibilitychange")); });
  await page.locator("#section-summary").getByText("The agent stopped and is waiting for your review.", { exact: true }).waitFor();
  await noSidewaysScroll(page, `${name} followed project`);
  await capture(page, `follow_${name}`);
}

// Ask first (Glass Box rule 1): the question leads. Continue opens the setup sheet with a title drawn from the question,
// and Start creates the project, saves the question and starts the agent. A request whose answer is lost is sent again
// with the same key, so a retry never makes a second project.
async function askFirst(page, mobile, name) {
  await reset();
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  const ask = page.getByRole("main").getByRole("form", { name: "Ask a research question" });
  const question = `Which diet raised plasma response most in the ${name} check, and by how much?`;
  const title = `Which diet raised plasma response most in the ${name} check`;
  await ask.getByRole("textbox", { name: "Ask a research question" }).fill(question);
  // The chip asks in another lab for this question only. A lab with no data set registered says so and cannot start.
  const chip = ask.getByRole("combobox", { name: "Lab for this question" });
  await chip.selectOption({ label: "In Example Field Lab" });
  await ask.getByText("No data and method set is registered for Example Field Lab yet, so the agent can't start research here.", { exact: true }).waitFor();
  assert.ok(await ask.getByRole("button", { name: "Continue", exact: true }).isDisabled(), `${name}: a lab with no data set cannot start research`);
  await chip.selectOption({ label: "In Example Nutrition Lab" });
  await ask.getByText("The agent can work from 3 sets in Example Nutrition Lab; you choose one, several or all of them next.", { exact: true }).waitFor();
  await ask.getByRole("button", { name: "Continue", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Set up the question" });
  assert.equal(await sheet.getByRole("textbox", { name: "Title", exact: true }).inputValue(), title, `${name}: the title is drawn from the question`);
  await sheet.getByText("Simulated diet study with the reviewed summary method", { exact: true }).waitFor();
  // A question across the lab: with several sets nothing is chosen until the reader picks, and
  // Every set chooses them all, with the data and methods they come to.
  assert.ok(await sheet.getByRole("button", { name: "Start", exact: true }).isDisabled(), `${name}: Start waits for a set`);
  await sheet.getByText("Choose at least one set.", { exact: true }).waitFor();
  await sheet.getByRole("checkbox", { name: /^Simulated intake records/ }).check();
  await sheet.getByText("The agent works from Simulated intake records with the reviewed intake summary.", { exact: true }).waitFor();
  const every = sheet.getByRole("checkbox", { name: /^Every set in Example Nutrition Lab/ });
  assert.equal(await every.evaluate((box) => box.indeterminate), true, `${name}: Every set shows that some are chosen`);
  await every.check();
  await sheet.getByText("The agent works across all 3 sets: 3 data sources and 3 methods.", { exact: true }).waitFor();
  await sheet.getByText("3 of 3 chosen", { exact: true }).waitFor();
  await noSidewaysScroll(page, `ask sheet ${name}`);
  await capture(page, `ask_sheet_${name}`);
  // The first create reaches the broker, but its answer is lost on the way back.
  let lost = false;
  const create = "**/api/v1/projects";
  await page.route(create, async (route) => {
    if (route.request().method() !== "POST" || lost) return route.continue();
    lost = true;
    await route.fetch();
    await route.abort("connectionreset");
  });
  await sheet.getByRole("button", { name: "Start", exact: true }).click();
  await sheet.getByText("The project was not created. Your question is still here; try again.", { exact: true }).waitFor();
  // Interception ends before the retry: removing a route while the next page's reads pass through it can hold one.
  await page.unroute(create);
  await sheet.getByRole("button", { name: "Start", exact: true }).click();
  await page.waitForURL((u) => /^#\/projects\/[0-9a-f-]{36}$/.test(u.hash));
  await page.getByRole("heading", { name: title, level: 1 }).waitFor();
  await page.getByText(/^Started\. You can leave this page/).waitFor();
  await page.getByText(question).first().waitFor();
  await page.getByRole("heading", { name: "Working now", level: 2 }).first().waitFor();
  // The project names every set it started from.
  const started = page.locator("dl.facts").filter({ hasText: "Started from" }).first();
  for (const set of ["Simulated diet study with the reviewed summary method", "Simulated intake records with the reviewed intake summary", "Simulated body composition scans"])
    await started.getByText(set, { exact: true }).waitFor();
  const made = await page.evaluate(async (t) => (await (await fetch("/api/v1/projects")).json()).projects.filter((p) => p.title === t).length, title);
  assert.equal(made, 1, `${name}: a request sent again made one project`);
  // The sidebar counts the lab's projects the broker's summaries say are working, the new one with them; other labs'
  // work is not counted in this lab.
  const workingNow = await page.evaluate(async () => (await (await fetch("/api/v1/projects")).json()).projects
    .filter((p) => p.group_key === "open_methods" && (["queued", "running", "waiting"].includes(p.latest_run?.status) || (p.task_counts?.active ?? 0) > 0)).length);
  if (!mobile) await page.getByRole("navigation", { name: "Places" }).getByRole("link", { name: `Working now, ${workingNow}`, exact: true }).waitFor();
  // New question from any page opens the sheet with the question first; Save without starting runs nothing.
  if (mobile) await page.getByRole("button", { name: "Open the sidebar" }).click();
  await page.getByRole("button", { name: "New question", exact: true }).first().click();
  assert.equal(await sheet.getByRole("textbox", { name: "Your question", exact: true }).inputValue(), "", `${name}: a new question starts empty`);
  await sheet.getByRole("textbox", { name: "Your question", exact: true }).fill("Do the two sensors agree after rain?");
  await sheet.getByRole("textbox", { name: "Title", exact: true }).fill(`Sensor agreement after rain, ${name}`);
  await sheet.getByRole("checkbox", { name: /^Simulated body composition scans/ }).check();
  await sheet.getByRole("button", { name: "Save without starting", exact: true }).click();
  await page.getByRole("heading", { name: `Sensor agreement after rain, ${name}`, level: 1 }).waitFor();
  await page.getByText(/^Saved\. Nothing runs until you start it/).waitFor();
  await page.getByRole("button", { name: "Start the research", exact: true }).waitFor();
}

// Where the workspace opens (Glass Box rule 1): a tab's first visit to the bare address lands on what
// needs the reader and says why; home stays one click away, and a later visit or a link opens where it points.
async function landing(viewport, colorScheme, name) {
  await reset();
  const context = await browser.newContext({ viewport, colorScheme });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(home(""), { waitUntil: "domcontentloaded" });
  await page.waitForURL((u) => u.hash === `#/projects/${main}`, { timeout: 10000 });
  await page.getByText("Opened Test diet response study: the agent asks for your review. Your research is in the sidebar.", { exact: true }).waitFor();
  await page.getByRole("heading", { name: "Test diet response study", level: 1 }).waitFor();
  await capture(page, `landing_${name}`);
  await go(page, viewport.width < 700, /^Your research/);
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  await page.waitForTimeout(800);
  assert.equal(new URL(page.url()).hash, "#/", `${name}: a tab lands once`);
  const other = await context.newPage();
  await other.goto(home("analyses"), { waitUntil: "domcontentloaded" });
  await other.waitForTimeout(1200);
  assert.equal(new URL(other.url()).hash, "#/analyses", `${name}: a link opens where it points`);
  assert.deepEqual(errors, [], `${name}: the landing page reported errors`);
  await context.close();
}

// Lab data: a table the lab keeps is sent from the project page, read by the broker's profile and shown as the
// sheet with the broker's reading on it; each choice starts filled in, the broker's code prepares the table, its receipt
// adds up, a refusal names the cell and the control that decides it, and the prepared table opens with figures.
async function labDataWalk(page, mobile, name) {
  const sample = await readFile(new URL("../fixtures/lab_data/gtt_week4_simulated.csv", import.meta.url));
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  const section = page.locator("#section-lab-data");
  await section.getByRole("heading", { name: /^Lab data/ }).waitFor();
  // An Excel 97-2003 file is blocked by the guard and the words say what to send instead.
  await page.locator("#lab-data-file").setInputFiles({ name: "old_meter.xls", mimeType: "application/vnd.ms-excel", buffer: Buffer.from("not a workbook") });
  await section.getByText("old_meter.xls: The upload guard blocked this file, so it was not taken: Excel 97-2003 files (.xls) are not read; save it as .xlsx or CSV and send that.", { exact: true }).waitFor();
  await section.getByText("Blocked by the upload guard").waitFor();
  // A table the guard takes opens on its own page once the broker has read it.
  await page.locator("#lab-data-file").setInputFiles({ name: "gtt_week4_simulated.csv", mimeType: "text/csv", buffer: sample });
  await page.waitForURL(/#\/projects\/[0-9a-f-]{36}\/data\/[0-9a-f-]{36}$/);
  await page.getByRole("heading", { name: "gtt_week4_simulated.csv", level: 1 }).waitFor();
  const sheet = page.getByRole("region", { name: "The sheet's cells" });
  await sheet.waitFor();
  await page.getByText("The table is rows 5 to 14, with names in row 3 and units in row 4.", { exact: true }).waitFor();
  // A quoted sentence keeps its own period.
  await page.getByText("Row 15 is a note below the table: “Note: readings above 600 mg/dL are the meter's ceiling.”", { exact: true }).waitFor();
  const rowHead = (n) => sheet.locator("tbody th").nth(n - 1);
  assert.equal((await rowHead(13).innerText()).replace(/\s+/g, " ").trim(), "13 Summary, left out", `${name}: the summary rows are left out to start`);
  assert.equal(await sheet.getByRole("button", { name: "A", exact: true }).getAttribute("aria-pressed"), "true");
  await page.getByText("7 choices are filled in from the sheet. Change any of them; the broker applies them as written.", { exact: true }).waitFor();
  await noSidewaysScroll(page, `lab data ${name}`);
  await capture(page, `lab_data_sheet_${name}`);
  // A row number leaves its row out and takes it back.
  await rowHead(8).getByRole("button", { name: "8" }).click();
  assert.equal((await rowHead(8).innerText()).replace(/\s+/g, " ").trim(), "8 Left out");
  await page.getByText("The table is saved as results/lab_data/gtt_week4_simulated.csv, with 35 rows and 10 columns:", { exact: false }).waitFor();
  await rowHead(8).getByRole("button", { name: "8" }).click();
  await page.getByText("with 40 rows and 10 columns:", { exact: false }).waitFor();
  // Values against a limit start as missing; insulin's are kept at their limit instead. Whole numbers cannot be halved.
  const insulin = page.getByRole("radiogroup", { name: "Values against a limit in Column I, insulin_0_min" });
  await insulin.getByRole("radio", { name: "Use the limit" }).check();
  const glucose = page.getByRole("radiogroup", { name: "Values against a limit in glucose, columns D to H" });
  assert.equal(await glucose.getByRole("radio", { name: "Half the limit" }).isDisabled(), true, `${name}: a whole-number column keeps whole numbers`);
  // Missing words: NA left unread stops the preparation at its cell, and the refusal leads back to the choice.
  await page.getByRole("checkbox", { name: "Read “NA” as missing" }).uncheck();
  await page.getByRole("button", { name: "Prepare the table" }).click();
  const receipt = page.locator(".receipt");
  await receipt.getByText("Not prepared: 1 cell no rule could read. Nothing was written.", { exact: true }).waitFor();
  await receipt.getByText("Row 8, column H: “NA” is not a number.", { exact: true }).waitFor();
  await receipt.getByRole("button", { name: "Choose how “NA” is read" }).click();
  const na = page.getByRole("checkbox", { name: "Read “NA” as missing" });
  await page.waitForFunction(() => document.activeElement?.getAttribute("data-token") === "NA");
  await na.check();
  await page.getByRole("button", { name: "Prepare the table" }).click();
  await receipt.getByText("Prepared · 40 rows").waitFor();
  await receipt.getByText("98 filled cells in the sheet: 18 outside the table, 8 in rows left out, 0 in columns not read, 72 read.", { exact: true }).waitFor();
  await receipt.getByText("10 rows read (5 to 14), 2 left out, 8 kept. 40 rows written, 5 for each kept row.", { exact: true }).waitFor();
  const changes = await receipt.locator(".changes li").allInnerTexts();
  assert.ok(changes.some((line) => /^insulin_0_min column I\s+7 values · 1 below a limit, written as the limit$/.test(line.replace(/\n/g, " ").replace(/ {2,}/g, " ").trim())), `${name}: insulin's limit is counted (${changes.join(" | ")})`);
  assert.ok(changes.some((line) => line.includes("3 recoded (“Female” to “female” 1, “MALE” to “male” 1 and “Male” to “male” 1)")), `${name}: each recode is counted`);
  // Both preparations stay listed, newest first; the prepared table opens in its result frame with figures.
  assert.equal(await page.locator(".versions li").count(), 2);
  await receipt.locator("p.facts", { hasText: "40 rows, 10 columns." }).waitFor();
  await noSidewaysScroll(page, `lab data prepared ${name}`);
  await capture(page, `lab_data_prepared_${name}`);
  await receipt.getByRole("button", { name: "Make a figure" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("radio", { name: /^Scatter plot/ }).waitFor();
  await dialog.locator(".stage .state").getByText("Preview, not saved", { exact: true }).waitFor();
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });
  // The project lists both files with where each stands; the lab's tables are not analyses in its counts.
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  await section.getByText("Prepared: 40 rows").waitFor();
  assert.equal(await section.locator(".files li").count(), 2);
}

// Reviewed R analyses: a prepared lab table starts on the tolerance test, filled in; its page leads with what ran,
// the headline statistics read from their file and the template's notes; the comparison its table is made for counts
// animals; a refusal names its setting and the step it asks for; a lab without the dataset method takes no lab data; the
// Detailed view lists the lab's files apart from the analyses.
async function analysisWalk(page, mobile, name) {
  const section = page.locator("#section-lab-data");
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  await section.getByRole("link", { name: "gtt_week4_simulated.csv" }).click();
  const receipt = page.locator(".receipt");
  await receipt.getByText("Prepared · 40 rows").waitFor();
  await receipt.getByRole("button", { name: "Analyze" }).click();
  const dialog = page.getByRole("dialog");
  const chosen = dialog.getByRole("radio", { name: /^Glucose or insulin tolerance test: areas per animal/ });
  await chosen.waitFor();
  assert.equal(await chosen.isChecked(), true, `${name}: a table in the long layout starts on the tolerance test`);
  // A setting by its name; the "needed" mark beside a name is not part of it.
  const setting = (label) => dialog.getByRole("combobox", { name: label, exact: true });
  assert.deepEqual([await setting("Animal").inputValue(), await setting("Time after the dose").inputValue(), await setting("Reading").inputValue(),
    await setting("Groups").inputValue()], ["mouse_id", "minutes", "glucose", "diet"], `${name}: the settings start from the prepared table`);
  await noSidewaysScroll(page, `analysis dialog ${name}`);
  await capture(page, `analysis_dialog_${name}`);
  await dialog.getByRole("button", { name: "Run this analysis" }).click();
  await page.waitForURL(/#\/projects\/[0-9a-f-]{36}\/analyses\/[0-9a-f-]{36}$/);
  const tolerance = page.url();
  const statement = page.locator(".statement");
  await statement.getByText("Ran with the settings as asked.", { exact: true }).waitFor({ timeout: 20000 });
  await statement.getByText(/^glucose tolerance test per animal: trapezoid areas over each animal's readings from 0 to 120 \(5 times\)/).waitFor();
  const stat = (key) => statement.locator(".stat").filter({ has: page.locator("dt", { hasText: new RegExp(`^${key}$`) }) }).locator("dd");
  await stat("animals_used").waitFor();
  assert.deepEqual([await stat("animals").innerText(), await stat("animals_used").innerText(), await stat("primary_endpoint").innerText()], ["8", "6", "incremental_auc"],
    `${name}: the headline statistics are read from statistics.csv`);
  await statement.getByText(/^6 of 8 animals used\. Listed and not used: 2 without one at the last time\./).waitFor();
  await page.getByText("Drawn by the reviewed R analysis template with its results, from the table's own values.", { exact: true }).waitFor();
  const settings = page.getByRole("region", { name: "Analysis settings" });
  await settings.getByText("A person, from this workspace", { exact: true }).waitFor();
  await noSidewaysScroll(page, `analysis tolerance ${name}`);
  await capture(page, `analysis_tolerance_${name}`);
  // The comparison the tolerance table is made for: animals, one row each, from this analysis's own results table.
  await statement.getByRole("button", { name: "Compare the 2 groups on incremental auc" }).click();
  const compare = dialog.getByRole("radio", { name: /^Group comparison \(t-test/ });
  await compare.waitFor();
  assert.equal(await compare.isChecked(), true);
  assert.deepEqual([await setting("Values").inputValue(), await setting("Groups").inputValue(), await setting("One row for each").inputValue()],
    ["incremental_auc", "diet", "mouse_id"], `${name}: the comparison counts animals`);
  await dialog.getByRole("button", { name: "Run this analysis" }).click();
  await page.waitForURL((url) => url.href !== tolerance);
  await statement.getByText("Ran with the settings as asked.", { exact: true }).waitFor({ timeout: 20000 });
  await stat("p_value").waitFor();
  const rounded = await stat("p_value").innerText();
  assert.match(rounded, /^\d(\.\d+)? × 10⁻[⁰¹²³⁴⁵⁶⁷⁸⁹]+$/, `${name}: a small p-value reads in scientific form, never 0 (${rounded})`);
  await statement.getByRole("button", { name: "Show every digit" }).click();
  assert.match(await stat("p_value").innerText(), /^0\.000\d{4,}/, `${name}: every digit on request`);
  await page.getByRole("heading", { name: "Incremental auc by diet", level: 1 }).waitFor();
  await page.getByText(/^Group comparison \(t-test \/ Wilcoxon\) of the results table from /).waitFor();
  await noSidewaysScroll(page, `analysis comparison ${name}`);
  await capture(page, `analysis_compare_${name}`);
  await page.goto(tolerance, { waitUntil: "domcontentloaded" });
  await page.locator(".made").filter({ hasText: "Analyses made from these results" }).getByRole("link", { name: "Incremental auc by diet" }).waitFor();
  // Readings of one animal are refused as animals: the refusal names its setting and offers the summary it asks for.
  await page.goto(home(`projects/${main}`), { waitUntil: "domcontentloaded" });
  await section.getByRole("link", { name: "gtt_week4_simulated.csv" }).click();
  await receipt.getByRole("button", { name: "Analyze" }).click();
  await dialog.getByRole("radio", { name: /^Group comparison \(t-test/ }).check();
  await setting("Values").selectOption("glucose");
  await setting("Groups").selectOption("diet");
  await setting("One row for each").selectOption("mouse_id");
  await dialog.getByRole("button", { name: "Run this analysis" }).click();
  await page.waitForURL(/\/analyses\/[0-9a-f-]{36}$/);
  await statement.getByText("Not run. 8 animals in mouse_id have more than one row; summarize each animal first, so animals and not hours or days are compared.", { exact: true })
    .waitFor({ timeout: 20000 });
  await noSidewaysScroll(page, `analysis refused ${name}`);
  await capture(page, `analysis_refused_${name}`);
  await statement.getByRole("button", { name: "Change “One row for each”" }).click();
  await dialog.locator('[data-setting="experimental_unit"].named').waitFor();
  await page.waitForFunction(() => document.activeElement?.closest("[data-setting]")?.getAttribute("data-setting") === "experimental_unit");
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });
  await statement.getByRole("button", { name: "Summarize each animal first" }).click();
  const summary = dialog.getByRole("radio", { name: /^Per-animal summary over a time window/ });
  await summary.waitFor();
  assert.equal(await summary.isChecked(), true);
  assert.equal(await setting("Animal").inputValue(), "mouse_id");
  const values = dialog.locator('[data-setting="values"]');
  assert.equal(await values.getByRole("checkbox", { name: "glucose", exact: true }).isChecked(), true, `${name}: the refused value is summarized`);
  assert.equal(await dialog.locator('[data-setting="keep"]').getByRole("checkbox", { name: "diet", exact: true }).isChecked(), true, `${name}: the groups are carried along`);
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "hidden" });
  // A lab whose dataset method is not granted takes no lab data (the per-lab signal, read as false when absent).
  await page.goto(home("projects/50000000-0000-4000-8000-000000000004"), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Field sampling plan", level: 1 }).waitFor();
  assert.equal(await page.locator("#lab-data-file").count(), 0, `${name}: no lab data is offered where the lab does not take it`);
  // The Detailed view lists the lab's files apart from the analyses.
  await page.goto(home(`workbench/projects/${main}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("tab", { name: "Overview" }).waitFor();
  if (mobile) await page.getByRole("navigation", { name: "Detailed view panes" }).getByRole("button", { name: "Explorer", exact: true }).click();
  const lab = page.getByRole("complementary", { name: "Explorer" }).getByRole("tree", { name: "Lab data" });
  await lab.getByRole("treeitem", { name: /^Lab file read, Finished/ }).waitFor();
  await lab.getByRole("treeitem", { name: /^Prepared table: gtt week4 simulated, Finished/ }).waitFor();
  await noSidewaysScroll(page, `detailed lab data ${name}`);
  await capture(page, `analysis_detailed_${name}`);
}

// Search your research (Command K) finds a project by its title or its question's words and places by name; Working
// now lists what an agent is working on across the reader's labs.
// Live data (the workspace should know when a change by a person or the agent makes a view out
// of date). The reader's own change reaches every view at once, their other tab included; the agent's reaches them
// through the change stream; reads the workspace already holds come back as 304; a renewed or dropped stream resumes;
// and a broker without the stream still shows the reader's own changes at once.
const fixtureReads = async () => (await fetch(new URL("/api/v1/fixture/reads", target))).json();
const fixture = (path) => fetch(new URL(`/api/v1/fixture/${path}`, target), { method: "POST", headers: { "X-Agent-Sandbox-Intent": "mutate" } });
async function settles(read, expected, what, ms = 4_000) {
  const end = Date.now() + ms;
  let value = await read();
  while (value !== expected && Date.now() < end) { await new Promise((done) => setTimeout(done, 100)); value = await read(); }
  assert.equal(value, expected, what);
}
async function liveData(page, context, mobile, name) {
  await reset();
  // A fresh document: a hash change alone would keep the lists read before the reset.
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await page.reload({ waitUntil: "domcontentloaded" });
  const needs = page.getByRole("main").locator("section[aria-labelledby='needs-title'] li.act-row");
  await needs.first().waitFor();
  // The lab page lists what waits in that lab only; another lab's paper stays in its own lab.
  const waitingNow = async () => (await (await fetch(new URL("/api/v1/inbox", target))).json()).items.filter((i) => i.lab === "open_methods");
  const before = await needs.count();
  assert.equal(before, Math.min((await waitingNow()).length, 5), `${name}: home starts from the broker's waiting list`);
  await settles(async () => (await fixtureReads()).streams >= 1, true, `${name}: the change stream connects`);
  // A second tab on Waiting for you.
  const other = await context.newPage();
  await other.goto(home("waiting"), { waitUntil: "domcontentloaded" });
  const otherItems = other.locator(".waiting-page .items > li");
  await otherItems.first().waitFor();
  const otherBefore = await otherItems.count();
  // The reader rejects a result in the first tab. The other tab, home and the counts follow with no reload.
  await page.goto(home(`projects/${main}/analyses/${firstTask}`), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Summarize each diet and test feature changes", level: 1 }).waitFor();
  await analysisTab(page, "Review");
  const form = page.locator("section[aria-labelledby='a-review'] form");
  await form.getByRole("radio", { name: /^Reject/ }).check();
  await form.getByLabel("Note", { exact: true }).fill(`The input is the pilot file, not the final data set (${name}).`);
  await form.getByRole("button", { name: "Record your review", exact: true }).click();
  await page.getByText("Your review is saved with this analysis.").first().waitFor();
  await settles(() => otherItems.count(), otherBefore - 1, `${name}: the other tab drops the reviewed result without a reload`, 2_000);
  await settles(async () => /^\((\d+)\)/.exec(await other.title())?.[1] ?? "0", String(otherBefore - 1), `${name}: the other tab's title counts one fewer`, 2_000);
  await go(page, mobile, /^Your research/);
  await settles(() => needs.count(), before - 1, `${name}: home shows one fewer without a reload`, 2_000);
  // The agent stops for review in another lab: a tab on that lab's Waiting for you shows it through the change stream
  // within a few seconds, and this lab's tab counts it on the lab button at once but leaves it off its lists.
  await other.goto(home("labs/field_trials"), { waitUntil: "domcontentloaded" });
  await other.getByRole("heading", { name: "Example Field Lab", level: 1 }).waitFor();
  await other.goto(home("waiting"), { waitUntil: "domcontentloaded" });
  await other.getByRole("heading", { name: "Waiting for you", level: 1 }).waitFor();
  await settles(async () => (await fixtureReads()).streams >= 2, true, `${name}: the Field Lab tab's stream connects`, 6_000);
  const asked = Date.now();
  await fixture("sensor-run-stops");
  const sensorItem = other.locator(".waiting-page .items > li", { hasText: "Soil moisture sensor comparison" });
  await sensorItem.waitFor({ timeout: 5_000 });
  assert.ok(Date.now() - asked < 5_000, `${name}: the agent's stop reaches the reader within seconds`);
  await other.getByText("One new item waits for you.", { exact: true }).waitFor({ state: "attached" });
  await page.locator("header.bar").getByRole("button", { name: "Change lab. You are in Example Nutrition Lab. 2 items wait for you in your other labs.", exact: true })
    .waitFor({ timeout: 5_000 });
  assert.equal(await page.getByRole("main").locator("section[aria-labelledby='needs-title'] li.act-row", { hasText: "Soil moisture sensor comparison" }).count(), 0,
    `${name}: another lab's stop stays off this lab's home`);
  await noSidewaysScroll(other, `live waiting ${name}`);
  await capture(other, `live_waiting_${name}`);
  // Reads the workspace holds come back unchanged.
  assert.ok((await fixtureReads()).notModified > 0, `${name}: unchanged reads come back as 304`);
  // A stream that reaches its lifetime resumes after its last event: a change made before it reconnects still arrives.
  const listReads = [];
  const countList = (request) => { if (new URL(request.url()).pathname === "/api/v1/projects") listReads.push(request.url()); };
  page.on("request", countList);
  await fixture("changes-renew");
  await fixture("long-run-advances");
  await settles(() => listReads.length > 0, true, `${name}: a change made while the stream renewed reaches home`, 6_000);
  page.off("request", countList);
  await settles(async () => (await fixtureReads()).streams >= 2, true, `${name}: both tabs reconnect after a renewal`, 6_000);
  // A dropped stream resumes the same way. The reader marks the agent's request as seen elsewhere (another device), so
  // only the stream can bring it to these tabs.
  await fixture("changes-drop");
  const sensorAsk = (await (await fetch(new URL("/api/v1/inbox", target))).json()).items.find((i) => i.project_title === "Soil moisture sensor comparison");
  const marked = await fetch(new URL("/api/v1/inbox/acknowledgements", target), { method: "POST",
    headers: { "Content-Type": "application/json", "X-Agent-Sandbox-Intent": "mutate" },
    body: JSON.stringify({ kind: sensorAsk.kind, ref: sensorAsk.ref, version: sensorAsk.version, request_id: randomUUID() }) });
  assert.equal(marked.status, 200);
  await other.locator(".waiting-page .items > li.seen", { hasText: "Soil moisture sensor comparison" }).waitFor({ timeout: 8_000 });
  await settles(async () => (await fixtureReads()).streams >= 2, true, `${name}: both tabs reconnect after a drop`, 8_000);
  // A reconnect that succeeds at once is not news; the sidebar names a stream only when it stays lost.
  const lostNote = page.getByRole("navigation", { name: "Places" }).getByText("Reconnecting for live updates. Lists update every minute meanwhile.", { exact: true });
  assert.equal(await lostNote.count(), 0, `${name}: a brief drop says nothing`);
  // A broker without the stream: the reader's own change still reaches every view at once. The second tab returns to
  // this lab's Waiting for you first.
  await other.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await other.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  await other.goto(home("waiting"), { waitUntil: "domcontentloaded" });
  await otherItems.first().waitFor();
  await fixture("changes?on=false");
  const seen = () => other.locator(".waiting-page .items > li.seen").count();
  const seenBefore = await seen();
  await go(page, mobile, /^Waiting for you/);
  const asking = page.locator(".waiting-page .items > li", { hasText: "Test diet response study" }).filter({ has: page.getByRole("button", { name: "Mark as seen" }) }).first();
  await asking.getByRole("button", { name: "Mark as seen", exact: true }).click();
  await settles(seen, seenBefore + 1, `${name}: without the stream the other tab still follows the reader's own mark`, 2_000);
  if (!mobile) await lostNote.waitFor({ timeout: 8_000 });
  await fixture("changes?on=true");
  await other.close();
}

async function searchAndWorking(page, mobile, name) {
  // Search and Working now cover the lab in view: another lab's project is found from its own lab only.
  await page.goto(home("labs/open_methods"), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Example Nutrition Lab", level: 1 }).waitFor();
  const open = () => mobile ? page.locator("header.bar").getByRole("button", { name: "Search your research" }).click() : page.keyboard.press("Control+K");
  await open();
  const search = page.getByRole("dialog", { name: "Search your research" });
  const field = search.getByRole("combobox");
  await field.fill("sensors agree");
  await page.waitForTimeout(400);
  assert.equal(await search.getByRole("option", { name: /Soil moisture sensor comparison/ }).count(), 0, `${name}: search stays in the lab in view`);
  await page.keyboard.press("Escape");
  await search.waitFor({ state: "hidden" });
  await page.goto(home("labs/field_trials"), { waitUntil: "domcontentloaded" });
  await page.getByRole("heading", { name: "Example Field Lab", level: 1 }).waitFor();
  await open();
  await field.fill("sensors agree");
  await search.getByRole("option", { name: /Soil moisture sensor comparison/ }).waitFor();
  await capture(page, `search_${name}`);
  await field.press("Enter");
  await page.getByRole("heading", { name: "Soil moisture sensor comparison", level: 1 }).waitFor();
  await open();
  await field.fill("working");
  await search.getByRole("option", { name: /^Go to Working now/ }).click();
  await page.getByRole("heading", { name: "Working now", level: 1 }).waitFor();
  await page.getByRole("main").getByRole("link", { name: "Open Soil moisture sensor comparison", exact: true }).waitFor();
  assert.equal(await page.getByRole("main").getByRole("link", { name: /Test diet response study/ }).count(), 0, `${name}: only working projects are listed`);
  await noSidewaysScroll(page, `working ${name}`);
  await capture(page, `working_${name}`);
}

try {
  for (const [name, viewport, colorScheme] of [
    ["desktop_light", { width: 1440, height: 1000 }, "light"],
    ["desktop_dark", { width: 1440, height: 1000 }, "dark"],
    ["mobile_light", { width: 390, height: 844 }, "light"],
    ["mobile_dark", { width: 390, height: 844 }, "dark"],
  ]) {
    await reset();
    const context = await browser.newContext({ viewport, colorScheme, acceptDownloads: true });
    // The landing rule moves a tab's first visit to the work that needs the reader; every walk here but landing() starts
    // from the address it names, as a reader who has already arrived would.
    await context.addInitScript(() => { try { sessionStorage.setItem("workspace_landed", "1"); } catch { /* none */ } });
    const page = await context.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    // Failed requests are judged by address below; the console line for them has no address.
    page.on("console", (message) => { if (message.type() === "error" && !message.text().startsWith("Failed to load resource")) errors.push(message.text()); });
    const failed = [];
    page.on("response", (response) => { if (response.status() >= 400) failed.push(`${response.status()} ${new URL(response.url()).pathname}`); });
    const mobile = viewport.width < 700;
    await workbench(page, mobile, name);
    await longRun(page, mobile, name);
    await liveLog(page, mobile, name);
    // The long run and the live log end in the first lab (its projects page, then its Detailed view start page) and the
    // results walk opens an analysis from there, so the lab walk below also checks that the home address opens Your
    // research after a lab's page.
    await resultFrames(page, mobile, name);
    await reset();
    await labShell(page, mobile, name);
    await searchAndWorking(page, mobile, name);
    await readProject(page, mobile, name);
    await correctedAnswer(page, name);
    await decisionInput(page, name);
    await editQuestionAndRun(page, name);
    await shareAccess(page);
    await downloadPackage(page);
    await readAnalysis(page, mobile, name);
    await readGeneratedAnalysis(page, name);
    await readConfiguredAnalysis(page, name);
    await readQueryAnalysis(page, name);
    await reviewAnalysis(page, name);
    await shareNote(page, mobile, name);
    await readLibrary(page, mobile, name);
    await readCommons(page, mobile, name);
    await reviewLinks(page, mobile, name);
    await startBlankProject(page, name);
    await displaySettings(page);
    await loadingInColumn(page, name);
    await skippedTransition(page);
    await inspect(page, mobile, name);
    await makeFigure(page, mobile, name);
    await followAProject(page, context, name);
    await askFirst(page, mobile, name);
    await landing(viewport, colorScheme, name);
    await acceptResult(page, name);
    await labDataWalk(page, mobile, name);
    await analysisWalk(page, mobile, name);
    await liveData(page, context, mobile, name);
    assert.deepEqual(errors, [], `${name}: the page reported errors`);
    // Expected refusals: the private project after its inspection ended, the three reads the walk fails on purpose, the
    // collection change whose answer the preview drops, and the change stream while the preview plays an older broker.
    const expected = new Set(["404 /favicon.ico", "404 /api/v1/changes/stream", `404 /api/v1/projects/${foreign}`, `503 /api/v1/tasks/${generatedTask}/artifacts`,
      `503 /api/v1/tasks/${generatedTask}/scientific-history`,
      `503 /api/v1/tasks/${standaloneTask}/lineage`, `503 /api/v1/projects/${main}`, "504 /api/v1/knowledge/commons/collections/baseline",
      "504 /api/v1/knowledge/relations/01920000-0000-7000-8000-00000000d102/review",
      "504 /api/v1/labs/open_methods/library/papers", "504 /api/v1/labs/open_methods/library/papers/80000000-0000-4000-8000-0000000000aa/acceptance"]);
    assert.deepEqual(failed.filter((entry) => !expected.has(entry)), [], `${name}: unexpected failed requests`);
    await context.close();
  }
  console.log("The Detailed view's Overview, explorer, thread, search and what needs attention, a long run paged, listed and followed as it moves on, waiting for a compute slot with its recent pace, watched as its record is written, asking a question first with a lost answer sent again, where a tab lands, search your research and Working now, the lab index with its facets, search and places, what waits for the reader and marking it as seen, lab letterhead and lab switching, project context on every analysis route, where each analysis came from with a failed read and retry, failed file reads with retry, lab data sent, read on its sheet, prepared with a refusal fixed and drawn from, reviewed R analyses run from the prepared table with the comparison of animals they lead to and a refusal's setting and first step, the project report with its figure plates and each planned step's saved history, a newer answer that replaces earlier findings, the exact input of a decision, an instruction's read line and its withdrawal, figure tracing, model-written code, a database query with its checks, a figure drawn by R from a result table with its versions, a person's review, sharing a note with the lab and the lab's shared notes, the lab library (search, adding a paper, labels, acceptance for the research agent and the papers waiting for it, and removal), the library papers the agent read and cited, the commons with a lead's baseline mark and its withdrawal, papers a page at a time with a marked paper's newer parts, a collection marked after a confirming step, a change whose answer was lost reloaded, and a member's read-only view, a lab's proposed links with a curator's acceptance and rejection and a member's read-only view, asking for changes and the research controls, the one-page project with what changed since the last look, where each value came from, its steps opened in place and accepting the result, sharing, downloads with the rerun kit, recorded steps, setup, display settings, inspection, a project followed live, instructed while it works and stopped with the tab hidden, and live data across two tabs (a review shown everywhere without a reload, the agent's stop within seconds, unchanged reads answered 304, a renewed and a dropped stream resumed, and a broker without the stream) passed in light and dark at desktop and phone widths.");
} finally {
  await browser.close();
  await reset().catch(() => undefined);
}

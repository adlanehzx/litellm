import { pathToFileURL } from "node:url";
import { writeFile } from "node:fs/promises";

const { chromium } = await import(pathToFileURL(process.env.QA_PLAYWRIGHT));
const mode = process.argv[2];
const browser = await chromium.launch({ executablePath: "/usr/bin/chromium", args: ["--no-sandbox"] });
const page = await browser.newPage({ viewport: { width: 2320, height: 980 }, locale: "en-US" });
const origin = "http://127.0.0.1:44058";

page.on("requestfailed", (request) => console.error("request failed", new URL(request.url()).pathname, request.failure()?.errorText));
page.on("pageerror", (error) => console.error("browser error", error.message));
page.on("response", async (response) => {
  if (response.url().includes("/spend/logs/")) console.log(response.status(), new URL(response.url()).pathname);
});
const highlight = async (locator) => {
  await locator.scrollIntoViewIfNeeded();
  await locator.evaluate((element) => {
    element.style.outline = "2px solid #dc2626";
    element.style.outlineOffset = "-1px";
  });
};
try {
  const login = await page.request.post(origin + "/login", {
    headers: { accept: "application/json" },
    form: { username: "admin", password: process.env.QA_UI_PASSWORD },
    maxRedirects: 0,
  });
  if (login.status() !== 303) throw new Error("Unexpected login HTTP status " + login.status());
  await page.goto(origin + "/ui/logs", { timeout: 90000, waitUntil: "domcontentloaded" });
  const dismiss = page.getByText("Don't ask me again", { exact: true });
  if (await dismiss.isVisible()) await dismiss.click();
  const table = page.locator("table").filter({ visible: true }).first();
  const sessionRow = table.getByRole("row").filter({ hasText: "cache-qa-replay" });
  const requestRow = table.getByRole("row").filter({ hasText: "cache-qa-single" });
  await sessionRow.waitFor({ timeout: 60000 });
  await requestRow.waitFor();
  const sessionTokens = sessionRow.getByRole("cell").filter({ hasText: "(3000+150)" });
  const requestTokens = requestRow.getByRole("cell").filter({ hasText: "(500+50)" });
  console.log("Session row:", await sessionTokens.innerText({timeout: 10000}));
  console.log("Request row:", await requestTokens.innerText({timeout: 10000}));
  if (mode === "after") {
    await sessionTokens.getByText("Prompt cache: 800 read / 800 written (40% of input read from cache)").waitFor();
    await requestTokens.getByText("Prompt cache: 350 read / 100 written").waitFor();
  } else if ((await sessionTokens.innerText()).includes("Prompt cache")) throw new Error("Unexpected prompt cache totals in baseline");
  await highlight(sessionTokens);
  await highlight(requestTokens);
  await page.screenshot({ path: new URL(mode + "-table.png", import.meta.url).pathname, animations: "disabled" });
  await sessionRow.getByText("cache-qa", { exact: true }).click();
  const drawer = page.getByRole("dialog").filter({ visible: true }).last();
  const cacheCounter = drawer.getByText(mode === "after" ? "1/3 response cache hits" : "1/3 cached", { exact: true });
  await cacheCounter.waitFor({timeout: 30000});
  await highlight(cacheCounter);
  if (mode === "after") {
    const drawerUsage = drawer.getByText("Prompt cache: 800 read / 800 written (40% of input read from cache)", { exact: true });
    await drawerUsage.waitFor();
    await highlight(drawerUsage);
    console.log("Session drawer:", await drawerUsage.innerText());
  } else console.log("Session drawer:", await cacheCounter.innerText());
  await page.screenshot({ path: new URL(mode + "-drawer.png", import.meta.url).pathname, animations: "disabled" });
} catch (error) {
  console.error("URL:", page.url());
  console.error((await page.locator("body").innerText()).slice(0, 4500));
  await page.screenshot({ path: new URL(mode + "-failure.png", import.meta.url).pathname });
  throw error;
} finally {
  await browser.close();
}

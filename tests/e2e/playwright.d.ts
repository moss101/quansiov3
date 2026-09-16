// `playwright` is a real-boundary dependency this workspace deliberately does not
// declare (QA-008's served-build tier only runs under QUANSIO_TEST_E2E=1, on a host
// that installs it separately -- see journeys.spec.ts). Without this stub `tsc`
// refuses the dynamic import with "Cannot find module" even though `it.skipIf` never
// lets it execute unless that host has actually installed the real package, whose
// real types then take over. Minimal on purpose: only the three calls
// journeys.spec.ts makes.
declare module "playwright" {
  interface Page {
    goto(url: string): Promise<unknown>;
    title(): Promise<string>;
  }
  interface Browser {
    newPage(): Promise<Page>;
    close(): Promise<void>;
  }
  interface BrowserType {
    launch(): Promise<Browser>;
  }
  export const chromium: BrowserType;
  export const firefox: BrowserType;
  export const webkit: BrowserType;
}

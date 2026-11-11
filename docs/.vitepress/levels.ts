// Reading levels for the docs. Each sidebar group belongs to one level, and
// the theme shows the level under the title of every page in the group, so a
// reader can tell an introduction from internals before reading further.
export type Level = "intro" | "guide" | "reference" | "advanced" | "background" | "contributor";

export const levels: Record<Level, { label: string; note: string }> = {
  intro: { label: "Introductory", note: "No Jupyter or Rust experience needed." },
  guide: { label: "Guide", note: "Day-to-day use. Assumes you've run Ganymede once." },
  reference: { label: "Reference", note: "Look things up as you need them." },
  advanced: { label: "Advanced", note: "How the server works inside. Not needed to use it." },
  background: { label: "Background", note: "Context: the design notes Ganymede was built from, and the Jupyter ecosystem it fits into." },
  contributor: { label: "Contributors", note: "For working on Ganymede itself." },
};

export interface Page {
  text: string;
  link: string;
  target?: string;
}

export interface Group {
  text: string;
  level: Level;
  items: Page[];
}

// ganymede-ui's pages, copied into docs/ganymede-ui/ by `npm run
// fetch-ui`, that the sidebar places by name. groups() leaves out any the
// copy lacks; config.mts lists any page the copy has and this doesn't in
// a group of its own, so a new page in ganymede-ui still shows.
export const uiPlaced: Record<string, string> = {
  apps: "The Mac, iPad and iPhone apps",
  shortcuts: "Keyboard shortcuts",
  device: "Device API",
  architecture: "ganymede-ui architecture",
  deploying: "Deploying ganymede-ui",
  releasing: "Releasing ganymede-ui",
};

function ui(name: string, has: (name: string) => boolean): Page[] {
  return has(name) ? [{ text: uiPlaced[name], link: `/ganymede-ui/${name}` }] : [];
}

// The sidebar, in reading order: grouped by what the reader is doing,
// whichever repository a page comes from. config.mts passes it to
// VitePress, and the theme looks pages up here to find their level. `has`
// says which of ganymede-ui's pages were copied in; the theme passes
// nothing and gets them all. Add a group here when its first page exists;
// an empty group renders as a dead heading.
export function groups(has: (name: string) => boolean = () => true): Group[] {
  return [
    {
      text: "Get started",
      level: "intro",
      items: [
        { text: "Overview", link: "/" },
        { text: "Install", link: "/install" },
        { text: "Getting started", link: "/getting-started" },
      ],
    },
    {
      text: "Use the apps",
      level: "guide",
      items: [
        { text: "Ganymede's app", link: "/app" },
        ...ui("apps", has),
        ...ui("shortcuts", has),
        ...ui("device", has),
      ],
    },
    {
      text: "Work with an agent",
      level: "guide",
      items: [
        { text: "Agents over MCP", link: "/mcp" },
        { text: "Agents with nb-cli", link: "/agents" },
        { text: "Live editing and document rooms", link: "/rooms" },
      ],
    },
    {
      text: "Use Jupyter tools",
      level: "guide",
      items: [
        { text: "JupyterLab and VS Code", link: "/frontends" },
        { text: "Other kernels", link: "/kernels" },
        { text: "Switching from jupyter_server", link: "/switching" },
      ],
    },
    {
      text: "Reference",
      level: "reference",
      items: [
        { text: "CLI and configuration", link: "/reference" },
        { text: "The API by example", link: "/api" },
        { text: "Known gaps", link: "/known-gaps" },
        { text: "Roadmap", link: "/roadmap" },
        { text: "Changelog", link: "/changelog" },
      ],
    },
    {
      // Generated from TSDoc in cdp-kernel and ganymede-device, copied in
      // by `npm run fetch-cell-api` (#596, #607).
      text: "Cell APIs",
      level: "reference",
      items: [
        { text: "jupyter", link: "/cell-api/jupyter" },
        { text: "Widgets and comms", link: "/cell-api/widgets" },
        { text: "device (macOS and iOS)", link: "/cell-api/device" },
      ],
    },
    {
      text: "How it works",
      level: "advanced",
      items: [
        { text: "Architecture", link: "/architecture" },
        { text: "Security", link: "/security" },
        { text: "Privacy and crash reporting", link: "/privacy" },
        { text: "Embedding", link: "/embedding" },
        { text: "Replacing Python", link: "/python-free" },
      ],
    },
    {
      text: "Background",
      level: "background",
      items: [
        { text: "Protocol and kernel host", link: "/protocol" },
        { text: "User interfaces", link: "/ui" },
        { text: "The iPad use case", link: "/ipados" },
        { text: "In-app kernels as server kernels", link: "/in-app-kernel" },
        { text: "A host API for kernels", link: "/host-api" },
        { text: "Other Jupyter clients", link: "/clients" },
        { text: "Prior art", link: "/prior-art" },
      ],
    },
    {
      text: "Contributing",
      level: "contributor",
      items: [
        { text: "Development", link: "/development" },
        { text: "Releasing", link: "/releasing" },
        ...ui("architecture", has),
        ...ui("deploying", has),
        ...ui("releasing", has),
        { text: "Development log", link: "/notes" },
      ],
    },
  ];
}

// The level of the page at `relativePath` ("install.md", "README.md"), if any.
export function levelOf(relativePath: string): Level | undefined {
  const link = "/" + relativePath.replace(/(^|\/)(README|index)\.md$/, "$1").replace(/\.md$/, "");
  return groups().find((g) => g.items.some((p) => p.link === link))?.level;
}

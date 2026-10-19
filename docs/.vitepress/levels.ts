// Reading levels for the docs. Each sidebar group belongs to one level, and
// the theme shows the level under the title of every page in the group, so a
// reader can tell an introduction from internals before reading further.
export type Level = "intro" | "guide" | "reference" | "advanced" | "background" | "contributor";

export const levels: Record<Level, { label: string; note: string }> = {
  intro: { label: "Introductory", note: "No Jupyter or Rust experience needed." },
  guide: { label: "Guide", note: "Day-to-day use. Assumes you've run Ganymede once." },
  reference: { label: "Reference", note: "Look things up as you need them." },
  advanced: { label: "Advanced", note: "How the server works inside. Not needed to use it." },
  background: { label: "Background", note: "Context: the Jupyter ecosystem Ganymede fits into." },
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

// The sidebar, in reading order. config.mts passes it to VitePress, and the
// theme looks pages up here to find their level. Add a group here when its
// first page exists; an empty group renders as a dead heading.
export function groups(): Group[] {
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
      text: "Guides",
      level: "guide",
      items: [
        { text: "JupyterLab and VS Code", link: "/frontends" },
        { text: "Ganymede's app", link: "/app" },
        { text: "Switching from jupyter_server", link: "/switching" },
        { text: "Security", link: "/security" },
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
      text: "How it works",
      level: "advanced",
      items: [
        { text: "Architecture", link: "/architecture" },
        { text: "Embedding", link: "/embedding" },
        { text: "Replacing Python", link: "/python-free" },
        { text: "Protocol and kernel host", link: "/protocol" },
        { text: "User interfaces", link: "/ui" },
        { text: "The iPad use case", link: "/ipados" },
      ],
    },
    {
      text: "Background",
      level: "background",
      items: [
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

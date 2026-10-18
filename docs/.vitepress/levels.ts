// Reading levels for the docs. Each sidebar group belongs to one level, and
// the theme shows the level under the title of every page in the group, so a
// reader can tell an introduction from internals before reading further.
export type Level = "intro" | "guide" | "reference" | "advanced" | "contributor";

export const levels: Record<Level, { label: string; note: string }> = {
  intro: { label: "Introductory", note: "No Jupyter or Rust experience needed." },
  guide: { label: "Guide", note: "Day-to-day use. Assumes you've run Ganymede once." },
  reference: { label: "Reference", note: "Look things up as you need them." },
  advanced: { label: "Advanced", note: "How the server works inside. Not needed to use it." },
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
      text: "Start here",
      level: "intro",
      items: [
        { text: "Overview", link: "/" },
        { text: "Install on a workstation", link: "/workstation" },
        { text: "Install", link: "/install" },
        { text: "Getting started", link: "/getting-started" },
      ],
    },
    {
      text: "Guides",
      level: "guide",
      items: [{ text: "JupyterLab and VS Code", link: "/frontends" }],
    },
    {
      text: "Design",
      level: "advanced",
      items: [
        { text: "Architecture", link: "/architecture" },
        { text: "Replacing Python", link: "/python-free" },
        { text: "Protocol and kernel host", link: "/protocol" },
        { text: "User interfaces", link: "/ui" },
        { text: "The iPad use case", link: "/ipados" },
      ],
    },
    {
      text: "Reference",
      level: "reference",
      items: [
        { text: "Security", link: "/security" },
        { text: "Roadmap", link: "/roadmap" },
        { text: "Prior art", link: "/prior-art" },
        { text: "Other Jupyter clients", link: "/clients" },
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

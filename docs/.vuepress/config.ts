import { viteBundler } from "@vuepress/bundler-vite";
import { registerComponentsPlugin } from "@vuepress/plugin-register-components";
import { path } from "@vuepress/utils";
import container from "markdown-it-container";
import { defineUserConfig } from "vuepress";
import { plumeTheme } from "vuepress-theme-plume";

export default defineUserConfig({
  base: "/",
  lang: "en-US",
  title: "telegram-tui",
  description:
    "A keyboard-driven Telegram client for the terminal. Arrows move, Enter selects, and every action a message supports is a labeled chip instead of a chord.",

  head: [
    [
      "meta",
      {
        name: "description",
        content:
          "tgt is a terminal Telegram client built on TDLib and ratatui. Two modifiers in the whole application, visible action chips instead of memorised chords, and both of its network egresses disclosed on the first run.",
      },
    ],
    ["link", { rel: "icon", type: "image/png", href: "/images/specht.png" }],
  ],

  bundler: viteBundler(),
  shouldPrefetch: false,

  // The engineering documents (the architecture contract, the build plan, the
  // design language, and the superpowers specs) stay in the repo for
  // contributors and are deliberately not part of the published site.
  pagePatterns: [
    "**/*.md",
    "!.vuepress",
    "!node_modules",
    "!architecture.md",
    "!plan.md",
    "!design-language.md",
    "!superpowers",
    "!superpowers/**",
  ],

  extendsMarkdown: (md) => {
    md.use(container, "terminal", {
      validate: (params: string) => {
        const info = params.trim();
        return /^terminal(?:\s+.*)?$/.test(info);
      },
      render: (tokens: any[], idx: number) => {
        const token = tokens[idx];
        if (token.nesting === 1) {
          const info = token.info.trim();
          const rest = info.replace(/^terminal\s*/, "");
          const attrs: Record<string, string> = {};
          const attrRegex = /(\w+)=((?:\"[^\"]*\")|(?:'[^']*')|(?:[^\s]+))/g;
          let consumed = "";
          let m: RegExpExecArray | null;
          while ((m = attrRegex.exec(rest)) !== null) {
            const key = m[1];
            let val = m[2];
            if (
              (val.startsWith('"') && val.endsWith('"')) ||
              (val.startsWith("'") && val.endsWith("'"))
            ) {
              val = val.slice(1, -1);
            }
            attrs[key] = val;
            consumed += m[0] + " ";
          }
          const positional = rest.replace(consumed, "").trim();
          const titleRaw = attrs.title ?? positional ?? "";
          const title = titleRaw ? md.utils.escapeHtml(titleRaw) : "";
          const titleAttr = title ? ` title=\"${title}\"` : "";
          return `\n<Terminal${titleAttr}>\n`;
        }
        return `\n</Terminal>\n`;
      },
    });

    md.use(container, "cast", {
      validate: (params: string) => {
        const info = params.trim();
        return /^cast(?:\s+.*)?$/.test(info);
      },
      render: (tokens: any[], idx: number) => {
        const token = tokens[idx];
        if (token.nesting === 1) {
          const info = token.info.trim();
          const rest = info.replace(/^cast\s*/, "");
          const attrs: Record<string, string> = {};
          const attrRegex = /(\w+)=((?:\"[^\"]*\")|(?:'[^']*')|(?:[^\s]+))/g;
          let m: RegExpExecArray | null;
          while ((m = attrRegex.exec(rest)) !== null) {
            const key = m[1];
            let val = m[2];
            if (
              (val.startsWith('"') && val.endsWith('"')) ||
              (val.startsWith("'") && val.endsWith("'"))
            ) {
              val = val.slice(1, -1);
            }
            attrs[key] = val;
          }

          const src = attrs.src ? md.utils.escapeHtml(attrs.src) : "";
          const title = attrs.title ? md.utils.escapeHtml(attrs.title) : "";
          const rows = attrs.rows ? Number.parseInt(attrs.rows, 10) : 16;
          const rowsAttr = Number.isFinite(rows) ? ` :rows="${rows}"` : "";
          const titleAttr = title ? ` title="${title}"` : "";
          // ClientOnly, not a bare <AsciinemaCast>: the real player (#77)
          // does browser-only work (DOM event delegation) at module-import
          // time, not just inside onMounted, so importing it at all crashes
          // VuePress's SSR pass in Node — confirmed by watching `mise run
          // docs-build` throw from inside asciinema-player's own module
          // during "Rendering N pages" and, worse, still exit 0 and produce
          // a page with nothing where the player should be. ClientOnly
          // skips SSR for its children entirely and mounts them only in the
          // browser, which is where a terminal player belongs anyway.
          return `\n<ClientOnly><AsciinemaCast src="${src}"${titleAttr}${rowsAttr} /></ClientOnly>\n`;
        }
        return "\n";
      },
    });
  },

  plugins: [
    registerComponentsPlugin({
      componentsDir: path.resolve(__dirname, "./components"),
    }),
  ],

  theme: plumeTheme({
    docsRepo: "https://github.com/SpechtLabs/telegram-tui",
    docsDir: "docs",
    docsBranch: "main",

    editLink: true,
    lastUpdated: false,
    contributors: false,

    cache: "filesystem",
    search: { provider: "local" },

    sidebar: {
      "/getting-started/": [
        {
          text: "Getting Started",
          icon: "mdi:rocket-launch",
          prefix: "/getting-started/",
          items: [
            { text: "What tgt is", link: "overview", icon: "mdi:eye" },
            {
              text: "Installation",
              link: "installation",
              icon: "mdi:download",
            },
            {
              text: "API credentials",
              link: "api-credentials",
              icon: "mdi:key-variant",
            },
            { text: "First login", link: "login", icon: "mdi:login-variant" },
            {
              text: "Quick Start",
              link: "quick",
              icon: "mdi:flash",
              badge: "2 min",
            },
          ],
        },
      ],

      "/guides/": [
        {
          text: "How-to Guides",
          icon: "mdi:compass",
          prefix: "/guides/",
          items: [
            {
              text: "Driving the client",
              icon: "mdi:keyboard",
              link: "keyboard",
              items: [
                {
                  text: "From the keyboard",
                  link: "keyboard",
                  icon: "mdi:keyboard-outline",
                },
                {
                  text: "Selection mode & chips",
                  link: "selection-mode",
                  icon: "mdi:cursor-default-click",
                },
                { text: "Using the mouse", link: "mouse", icon: "mdi:mouse" },
                {
                  text: "Search & the palette",
                  link: "search-and-palette",
                  icon: "mdi:magnify",
                },
              ],
            },
            {
              text: "Appearance & media",
              icon: "mdi:palette",
              link: "themes",
              items: [
                { text: "Themes", link: "themes", icon: "mdi:palette-outline" },
                {
                  text: "Media, downloads & inline images",
                  link: "media",
                  icon: "mdi:image-multiple",
                },
              ],
            },
            {
              text: "Privacy",
              icon: "mdi:shield-account",
              link: "telemetry",
              items: [
                {
                  text: "Telemetry controls",
                  link: "telemetry",
                  icon: "mdi:chart-box-outline",
                },
              ],
            },
          ],
        },
      ],

      "/understanding/": [
        {
          text: "Understanding tgt",
          icon: "mdi:lightbulb",
          collapsed: false,
          prefix: "/understanding/",
          items: [
            {
              text: "The shape of the app",
              link: "architecture",
              icon: "mdi:shape-outline",
            },
            {
              text: "Why chat order mirrors TDLib",
              link: "chat-order",
              icon: "mdi:sort-variant",
            },
            {
              text: "History paging",
              link: "history-paging",
              icon: "mdi:page-previous-outline",
            },
            {
              text: "Telemetry by construction",
              link: "telemetry-allowlist",
              icon: "mdi:shield-lock",
            },
            {
              text: "Contributing",
              link: "contributing",
              icon: "mdi:source-pull",
            },
          ],
        },
      ],

      "/reference/": [
        {
          text: "Reference",
          icon: "mdi:book",
          collapsed: false,
          prefix: "/reference/",
          items: [
            { text: "Keymap", link: "keymap", icon: "mdi:keyboard-variant" },
            {
              text: "Configuration",
              link: "configuration",
              icon: "mdi:file-cog",
            },
            { text: "CLI Reference", link: "cli", icon: "mdi:terminal" },
            {
              text: "Theme tokens",
              link: "theme-tokens",
              icon: "mdi:palette-swatch",
            },
          ],
        },
      ],
    },

    markdown: {
      collapse: true,
      timeline: true,
      plot: true,
      mermaid: true,
      image: {
        figure: true,
        lazyload: true,
        mark: true,
        size: true,
      },
    },

    watermark: false,
  }),
});

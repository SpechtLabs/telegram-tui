import { defineNavbarConfig } from "vuepress-theme-plume";

export const navbar = defineNavbarConfig([
  { text: "Home", link: "/", icon: "mdi:home" },

  {
    text: "Getting Started",
    icon: "mdi:rocket-launch",
    items: [
      { text: "What tgt is", link: "/getting-started/overview", icon: "mdi:eye" },
      {
        text: "Installation",
        link: "/getting-started/installation",
        icon: "mdi:download",
      },
      {
        text: "API credentials",
        link: "/getting-started/api-credentials",
        icon: "mdi:key-variant",
      },
      {
        text: "First login",
        link: "/getting-started/login",
        icon: "mdi:login-variant",
      },
      {
        text: "Quick Start",
        link: "/getting-started/quick",
        icon: "mdi:flash",
      },
    ],
  },

  {
    text: "Guides",
    icon: "mdi:compass",
    items: [
      {
        text: "From the keyboard",
        link: "/guides/keyboard",
        icon: "mdi:keyboard-outline",
      },
      {
        text: "Selection mode & chips",
        link: "/guides/selection-mode",
        icon: "mdi:cursor-default-click",
      },
      { text: "Using the mouse", link: "/guides/mouse", icon: "mdi:mouse" },
      {
        text: "Search & the palette",
        link: "/guides/search-and-palette",
        icon: "mdi:magnify",
      },
      { text: "Themes", link: "/guides/themes", icon: "mdi:palette-outline" },
      {
        text: "Media & inline images",
        link: "/guides/media",
        icon: "mdi:image-multiple",
      },
      {
        text: "Telemetry controls",
        link: "/guides/telemetry",
        icon: "mdi:chart-box-outline",
      },
    ],
  },

  {
    text: "Understanding",
    icon: "mdi:lightbulb",
    items: [
      {
        text: "The shape of the app",
        link: "/understanding/architecture",
        icon: "mdi:shape-outline",
      },
      {
        text: "Why chat order mirrors TDLib",
        link: "/understanding/chat-order",
        icon: "mdi:sort-variant",
      },
      {
        text: "History paging",
        link: "/understanding/history-paging",
        icon: "mdi:page-previous-outline",
      },
      {
        text: "Telemetry by construction",
        link: "/understanding/telemetry-allowlist",
        icon: "mdi:shield-lock",
      },
      {
        text: "Contributing",
        link: "/understanding/contributing",
        icon: "mdi:source-pull",
      },
    ],
  },

  {
    text: "Reference",
    icon: "mdi:book",
    items: [
      { text: "Keymap", link: "/reference/keymap", icon: "mdi:keyboard-variant" },
      {
        text: "Configuration",
        link: "/reference/configuration",
        icon: "mdi:file-cog",
      },
      { text: "CLI Reference", link: "/reference/cli", icon: "mdi:terminal" },
      {
        text: "Theme tokens",
        link: "/reference/theme-tokens",
        icon: "mdi:palette-swatch",
      },
    ],
  },

  {
    text: "More",
    icon: "mdi:dots-horizontal",
    items: [
      {
        text: "Download",
        link: "https://github.com/SpechtLabs/telegram-tui/releases",
        target: "_blank",
        rel: "noopener noreferrer",
        icon: "mdi:download",
      },
      {
        text: "Report an Issue",
        link: "https://github.com/SpechtLabs/telegram-tui/issues/new/choose",
        target: "_blank",
        rel: "noopener noreferrer",
        icon: "mdi:bug-outline",
      },
    ],
  },
]);

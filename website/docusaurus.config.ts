import { themes as prismThemes } from 'prism-react-renderer';
import type { Config } from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'ThoughtGate',
  tagline: 'Human-in-the-loop approval workflows for MCP agents',
  favicon: 'img/favicon.ico',

  url: 'https://thoughtgate.io',
  baseUrl: process.env.DOCUSAURUS_BASE_URL || '/',

  organizationName: 'thoughtgate',
  projectName: 'thoughtgate',
  trailingSlash: false,

  onBrokenLinks: 'throw',
  onBrokenAnchors: 'throw',
  onBrokenMarkdownLinks: 'throw',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          editUrl: 'https://github.com/thoughtgate/thoughtgate/tree/main/website/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    navbar: {
      title: 'ThoughtGate',
      logo: {
        alt: 'ThoughtGate Logo',
        src: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          href: 'https://github.com/thoughtgate/thoughtgate',
          label: 'GitHub',
          position: 'right',
        },
        {
          href: 'https://bencher.dev/perf/thoughtgate',
          label: 'Performance',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Documentation',
          items: [
            { label: 'Introduction', to: '/docs' },
            { label: 'Wrap Your First Agent', to: '/docs/tutorials/wrap-first-agent' },
            { label: 'Quickstart', to: '/docs/how-to/quickstart' },
            { label: 'Supported Agents', to: '/docs/reference/supported-agents' },
            { label: 'Architecture', to: '/docs/explanation/architecture' },
          ],
        },
        {
          title: 'Community',
          items: [
            {
              label: 'GitHub Discussions',
              href: 'https://github.com/thoughtgate/thoughtgate/discussions',
            },
            {
              label: 'Issues',
              href: 'https://github.com/thoughtgate/thoughtgate/issues',
            },
          ],
        },
        {
          title: 'More',
          items: [
            {
              label: 'GitHub',
              href: 'https://github.com/thoughtgate/thoughtgate',
            },
            {
              label: 'Performance Dashboard',
              href: 'https://bencher.dev/perf/thoughtgate',
            },
          ],
        },
      ],
      copyright: `Copyright ${new Date().getFullYear()} ThoughtGate. Built with Docusaurus.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['rust', 'bash', 'yaml', 'toml', 'json'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;

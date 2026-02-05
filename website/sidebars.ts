import type { SidebarsConfig } from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docsSidebar: [
    'intro',
    {
      type: 'category',
      label: 'Tutorials',
      items: [
        'tutorials/wrap-first-agent',
        'tutorials/first-proxy',
      ],
    },
    {
      type: 'category',
      label: 'How-to Guides',
      items: [
        'how-to/quickstart',
        'how-to/install',
        'how-to/wrap-agents',
        'how-to/write-policies',
        'how-to/use-profiles',
        'how-to/deploy-kubernetes',
        'how-to/monitor',
        'how-to/troubleshoot',
      ],
    },
    {
      type: 'category',
      label: 'Reference',
      items: [
        'reference/cli',
        'reference/configuration',
        'reference/supported-agents',
        'reference/policy-syntax',
        'reference/error-codes',
        'reference/telemetry',
      ],
    },
    {
      type: 'category',
      label: 'Explanation',
      items: [
        'explanation/why-thoughtgate',
        'explanation/architecture',
        'explanation/traffic-tiers',
        'explanation/security-model',
      ],
    },
  ],
};

export default sidebars;

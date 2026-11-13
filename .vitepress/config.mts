import { defineConfig } from "vitepress";
import llmstxt from "vitepress-plugin-llms";

export default defineConfig({
  vite: {
    plugins: [llmstxt()],
  },
  title: "cuora",
  description: "secrets broker for agents — sealed, not spilled",
  base: "/cuora/",
  srcExclude: ["README.md"],
  ignoreDeadLinks: [/\/api\//],
  themeConfig: {
    nav: [
      { text: "Getting Started", link: "/getting-started" },
      { text: "Tutorial", link: "/tutorials/first-secret" },
      { text: "How-to", link: "/how-to/add-a-secret" },
      { text: "Explanation", link: "/explanation/architecture" },
      { text: "Reference", link: "/reference/cli" },
      {
        text: "LLMs",
        items: [
          { text: "llms.txt", link: "https://9aia.com/cuora/llms.txt", target: "_blank" },
          { text: "llms-full.txt", link: "https://9aia.com/cuora/llms-full.txt", target: "_blank" },
        ],
      },
    ],
    sidebar: [
      {
        text: "Tutorials",
        items: [{ text: "Your first secret", link: "/tutorials/first-secret" }],
      },
      {
        text: "How-to",
        items: [
          { text: "Add a secret", link: "/how-to/add-a-secret" },
          { text: "Inject into a process", link: "/how-to/inject-into-a-process" },
          { text: "Rotate a secret", link: "/how-to/rotate-a-secret" },
          { text: "Route envs to vaults", link: "/how-to/route-envs-to-vaults" },
          { text: "Cache a remote vault", link: "/how-to/cached-vaults" },
          { text: "Deploy to a sink", link: "/how-to/deploy-to-a-sink" },
          { text: "Guardrails for agents", link: "/how-to/guardrails-for-agents" },
          { text: "Cloudflare Worker setup", link: "/how-to/cloudflare-worker-setup" },
          { text: "Ship secrets as a sealed pack", link: "/how-to/sealed-packs" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "CLI", link: "/reference/cli" },
          { text: "Config", link: "/reference/config" },
          { text: "Audit events", link: "/reference/audit-events" },
          { text: "Serve protocol", link: "/reference/serve-protocol" },
          { text: "Environment variables", link: "/reference/env-vars" },
          { text: "Crate API", link: "/reference/crate-api" },
        ],
      },
      {
        text: "Explanation",
        items: [
          { text: "Architecture", link: "/explanation/architecture" },
          { text: "Threat model", link: "/explanation/threat-model" },
          { text: "Boundaries", link: "/explanation/boundaries" },
          { text: "Crypto model", link: "/explanation/crypto-model" },
          { text: "The worktree model", link: "/explanation/worktree-model" },
        ],
      },
    ],
    socialLinks: [{ icon: "github", link: "https://github.com/9aia/cuora" }],
  },
});

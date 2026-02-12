import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: <span className="font-mono font-bold">pm3</span>,
    },
    githubUrl: "https://github.com/frectonz/pm3",
  };
}

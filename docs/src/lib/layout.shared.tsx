import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <span className="flex items-center gap-2">
          <img src="/logo.svg" alt="pm3 logo" className="size-6" />
          <span className="font-mono font-bold">pm3</span>
        </span>
      ),
    },
    githubUrl: "https://github.com/frectonz/pm3",
  };
}

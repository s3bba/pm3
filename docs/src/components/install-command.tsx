"use client";

import { useState } from "react";
import { CopyButton } from "@/components/copy-button";

const commands = {
  unix: "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/frectonz/pm3/releases/download/0.1.6/pm3-installer.sh | sh",
  windows:
    'powershell -ExecutionPolicy ByPass -c "irm https://github.com/frectonz/pm3/releases/download/0.1.6/pm3-installer.ps1 | iex"',
} as const;

type Tab = keyof typeof commands;

export function InstallCommand() {
  const [tab, setTab] = useState<Tab>("unix");

  return (
    <div className="w-full max-w-4xl mb-8  border border-fd-border bg-fd-card p-4 text-left overflow-hidden">
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2">
          <span className="w-3 h-3  bg-red-500" />
          <span className="w-3 h-3  bg-yellow-500" />
          <span className="w-3 h-3  bg-green-500" />
          <div className="flex ml-2 gap-1">
            {(["unix", "windows"] as const).map((t) => (
              <button
                key={t}
                type="button"
                onClick={() => setTab(t)}
                className={`px-2.5 py-0.5text-xs font-mono transition-colors ${
                  tab === t
                    ? "bg-fd-accent text-fd-foreground"
                    : "text-fd-muted-foreground hover:text-fd-foreground"
                }`}
              >
                {t === "unix" ? "Unix" : "Windows"}
              </button>
            ))}
          </div>
        </div>
        <CopyButton text={commands[tab]} />
      </div>
      <pre className="font-mono text-sm text-fd-foreground overflow-x-auto">
        <span className="text-fd-muted-foreground">$ </span>
        {commands[tab]}
      </pre>
    </div>
  );
}

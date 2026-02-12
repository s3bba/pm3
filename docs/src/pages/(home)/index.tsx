import { ArrowRight, Wrench } from "lucide-react";
import { Link } from "waku";
import { InstallCommand } from "@/components/install-command";

const features = [
  {
    title: "Simple Config",
    description:
      "Define your processes in a single pm3.toml file. No complex setup — just TOML.",
  },
  {
    title: "Smart Restarts",
    description:
      "Exponential backoff, health checks, memory limits — your processes stay up.",
  },
  {
    title: "Interactive TUI",
    description:
      "Monitor everything in real-time from your terminal with a full-featured TUI.",
  },
];

const exampleToml = `[web]
command = "node server.js"
cwd = "./frontend"
env = { PORT = "3000" }
health_check = "http://localhost:3000/health"

[api]
command = "python app.py"
restart = "always"
depends_on = ["web"]

[worker]
command = "node worker.js"
max_memory = "512M"
cron_restart = "0 3 * * *"`;

const exampleOutput = `┌────────┬───────┬───────┬────────┬──────┬──────┬────────┬──────────┐
│ name   │ group │ pid   │ status │ cpu  │ mem  │ uptime │ restarts │
├────────┼───────┼───────┼────────┼──────┼──────┼────────┼──────────┤
│ web    │ -     │ 42150 │ online │ 1.2% │ 5.2M │ 2m 13s │ 0        │
│ api    │ -     │ 42153 │ online │ 0.8% │ 3.1M │ 2m 10s │ 0        │
│ worker │ -     │ 42156 │ online │ 0.5% │ 2.8M │ 2m 10s │ 1        │
└────────┴───────┴───────┴────────┴──────┴──────┴────────┴──────────┘`;

export default function Home() {
  return (
    <div className="flex flex-col min-h-screen">
      <title>pm3 - A modern process manager</title>
      {/* Hero */}
      <section className="flex flex-col items-center justify-center px-4 py-24 md:py-32 text-center">
        <h1 className="font-mono font-bold text-5xl md:text-7xl mb-4">pm3</h1>
        <p className="font-mono text-fd-muted-foreground text-lg md:text-xl mb-8 max-w-lg">
          A modern process manager.
        </p>

        <InstallCommand />

        <div className="flex gap-4 flex-wrap justify-center">
          <Link
            to="/docs/quick-start"
            className="flex items-center gap-2 px-6 py-3 bg-fd-primary text-fd-primary-foreground font-medium text-sm"
          >
            Get Started
            <ArrowRight className="w-4 h-4" />
          </Link>
          <Link
            to="/config-builder"
            className="flex items-center gap-2 px-6 py-3 border border-fd-border text-fd-foreground font-medium text-sm hover:bg-fd-accent transition-colors"
          >
            <Wrench className="w-4 h-4" />
            Config Builder
          </Link>
        </div>
      </section>

      {/* Features */}
      <section className="px-4 py-16 max-w-5xl mx-auto w-full">
        <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
          {features.map((feature) => (
            <div
              key={feature.title}
              className=" border border-fd-border bg-fd-card p-6"
            >
              <h3 className="font-semibold text-fd-foreground mb-2">
                {feature.title}
              </h3>
              <p className="text-sm text-fd-muted-foreground">
                {feature.description}
              </p>
            </div>
          ))}
        </div>
      </section>

      {/* Quick Example */}
      <section className="px-4 py-16 max-w-5xl mx-auto w-full">
        <h2 className="font-mono font-bold text-2xl md:text-3xl text-center mb-8">
          Define. Start. Monitor.
        </h2>
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          <div className=" border border-fd-border bg-fd-card overflow-hidden">
            <div className="px-4 py-2 border-b border-fd-border text-xs font-mono text-fd-muted-foreground">
              pm3.toml
            </div>
            <pre className="p-4 font-mono text-sm text-fd-foreground overflow-x-auto">
              {exampleToml}
            </pre>
          </div>
          <div className=" border border-fd-border bg-fd-card overflow-hidden">
            <div className="px-4 py-2 border-b border-fd-border text-xs font-mono text-fd-muted-foreground">
              $ pm3 list
            </div>
            <pre className="p-4 font-mono text-sm text-fd-foreground overflow-x-auto">
              {exampleOutput}
            </pre>
          </div>
        </div>
      </section>

      {/* Footer */}
      <footer className="mt-auto border-t border-fd-border px-4 py-8">
        <div className="max-w-5xl mx-auto flex flex-col md:flex-row justify-between gap-8">
          <div>
            <span className="font-mono font-bold text-fd-foreground">pm3</span>
            <p className="text-sm text-fd-muted-foreground mt-1">
              A modern process manager.
            </p>
          </div>
          <div className="flex gap-12">
            <div>
              <h4 className="font-medium text-sm text-fd-foreground mb-2">
                Documentation
              </h4>
              <ul className="space-y-1">
                <li>
                  <Link
                    to="/docs/quick-start"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    Quick Start
                  </Link>
                </li>
                <li>
                  <Link
                    to="/docs/configuration"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    Configuration
                  </Link>
                </li>
                <li>
                  <Link
                    to="/docs/cli"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    CLI Reference
                  </Link>
                </li>
              </ul>
            </div>
            <div>
              <h4 className="font-medium text-sm text-fd-foreground mb-2">
                Links
              </h4>
              <ul className="space-y-1">
                <li>
                  <a
                    href="https://github.com/frectonz/pm3"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    GitHub
                  </a>
                </li>
                <li>
                  <a
                    href="https://github.com/frectonz/pm3/releases"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    Releases
                  </a>
                </li>
                <li>
                  <a
                    href="https://github.com/frectonz/pm3/blob/main/LICENSE"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-sm text-fd-muted-foreground hover:text-fd-foreground"
                  >
                    MIT License
                  </a>
                </li>
              </ul>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}

export const getConfig = async () => {
  return {
    render: "static",
  };
};

import {
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative } from "node:path";
import { ImageResponse } from "@takumi-rs/image-response";

const CONTENT_DIR = join(import.meta.dirname, "../content/docs");
const OUTPUT_DIR = join(import.meta.dirname, "../public/og/docs");

interface Page {
  slug: string;
  title: string;
  description?: string;
}

function parseFrontmatter(content: string): {
  title?: string;
  description?: string;
} {
  const match = content.match(/^---\n([\s\S]*?)\n---/);
  if (!match) return {};

  const frontmatter: Record<string, string> = {};
  for (const line of match[1].split("\n")) {
    const sep = line.indexOf(":");
    if (sep === -1) continue;
    const key = line.slice(0, sep).trim();
    const value = line
      .slice(sep + 1)
      .trim()
      .replace(/^["']|["']$/g, "");
    frontmatter[key] = value;
  }

  return frontmatter;
}

function collectPages(dir: string): Page[] {
  const pages: Page[] = [];

  for (const entry of readdirSync(dir)) {
    const fullPath = join(dir, entry);
    const stat = statSync(fullPath);

    if (stat.isDirectory()) {
      pages.push(...collectPages(fullPath));
    } else if (entry.endsWith(".mdx") || entry.endsWith(".md")) {
      const content = readFileSync(fullPath, "utf-8");
      const { title, description } = parseFrontmatter(content);
      if (!title) continue;

      const rel = relative(CONTENT_DIR, fullPath)
        .replace(/\.mdx?$/, "")
        .replace(/\/index$/, "");

      pages.push({ slug: rel, title, description });
    }
  }

  return pages;
}

function gridLines(width: number, height: number, cellSize: number) {
  const lines: React.JSX.Element[] = [];

  for (let x = cellSize; x < width; x += cellSize) {
    const dist = Math.abs(x - width * 0.4) / width;
    const opacity = Math.max(0, 0.12 - dist * 0.15);
    if (opacity <= 0) continue;
    lines.push(
      <div
        key={`v${x}`}
        style={{
          position: "absolute",
          left: x,
          top: 0,
          width: "1px",
          height: "100%",
          backgroundColor: `rgba(40, 200, 64, ${opacity})`,
        }}
      />,
    );
  }

  for (let y = cellSize; y < height; y += cellSize) {
    const dist = y / height;
    const opacity = Math.max(0, 0.12 - dist * 0.14);
    if (opacity <= 0) continue;
    lines.push(
      <div
        key={`h${y}`}
        style={{
          position: "absolute",
          top: y,
          left: 0,
          height: "1px",
          width: "100%",
          backgroundColor: `rgba(40, 200, 64, ${opacity})`,
        }}
      />,
    );
  }

  return lines;
}

async function generateOGImage(page: Page): Promise<void> {
  const response = new ImageResponse(
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        width: "100%",
        height: "100%",
        backgroundColor: "#0a0a0a",
        fontFamily: "Geist, sans-serif",
        position: "relative",
      }}
    >
      {/* Grid pattern overlay */}
      <div
        style={{
          display: "flex",
          position: "absolute",
          inset: 0,
        }}
      >
        {gridLines(1200, 630, 48)}
      </div>

      {/* Green accent bar at top */}
      <div
        style={{ display: "flex", height: "4px", backgroundColor: "#28c840" }}
      />

      <div
        style={{
          display: "flex",
          flexDirection: "column",
          justifyContent: "space-between",
          flex: 1,
          padding: "48px 64px",
        }}
      >
        {/* Logo: mini terminal */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "20px",
          }}
        >
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              gap: "8px",
              padding: "12px 16px",
              backgroundColor: "#141414",
              border: "1.5px solid #2a2a2a",
            }}
          >
            <div
              style={{
                display: "flex",
                fontFamily: "Geist Mono, monospace",
                fontSize: "18px",
              }}
            >
              <span style={{ color: "#666" }}>$&nbsp;</span>
              <span style={{ color: "#e0e0e0", fontWeight: 700 }}>pm3</span>
            </div>
            <div style={{ display: "flex", gap: "6px" }}>
              <div
                style={{
                  display: "flex",
                  width: "8px",
                  height: "8px",
                  borderRadius: "50%",
                  backgroundColor: "#28c840",
                }}
              />
              <div
                style={{
                  display: "flex",
                  width: "8px",
                  height: "8px",
                  borderRadius: "50%",
                  backgroundColor: "#28c840",
                }}
              />
              <div
                style={{
                  display: "flex",
                  width: "8px",
                  height: "8px",
                  borderRadius: "50%",
                  backgroundColor: "#28c840",
                }}
              />
            </div>
          </div>

          <div
            style={{
              display: "flex",
              fontSize: "18px",
              fontFamily: "Geist Mono, monospace",
              color: "#555",
              letterSpacing: "0.05em",
            }}
          >
            DOCUMENTATION
          </div>
        </div>

        {/* Title + description */}
        <div style={{ display: "flex", flexDirection: "column", gap: "16px" }}>
          <div
            style={{
              fontSize: page.title.length > 30 ? "48px" : "60px",
              fontWeight: 700,
              lineHeight: 1.15,
              color: "#fafafa",
              letterSpacing: "-0.03em",
            }}
          >
            {page.title}
          </div>
          {page.description && (
            <div
              style={{
                fontSize: "22px",
                color: "#71717a",
                lineHeight: 1.5,
                maxWidth: "900px",
              }}
            >
              {page.description}
            </div>
          )}
        </div>

        {/* Bottom bar */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
          }}
        >
          <div
            style={{
              display: "flex",
              fontFamily: "Geist Mono, monospace",
              fontSize: "16px",
              color: "#3f3f46",
            }}
          >
            pm3.frectonz.io
          </div>
          <div
            style={{
              display: "flex",
              fontFamily: "Geist Mono, monospace",
              fontSize: "16px",
              color: "#3f3f46",
            }}
          >
            A modern process manager
          </div>
        </div>
      </div>
    </div>,
    { width: 1200, height: 630, format: "png" },
  );

  const buffer = Buffer.from(await response.arrayBuffer());
  const outputPath = join(OUTPUT_DIR, `${page.slug}.png`);

  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, buffer);
  console.log(`  ${page.slug}.png`);
}

async function main() {
  const pages = collectPages(CONTENT_DIR);
  console.log(`Generating ${pages.length} OG images...`);

  for (const page of pages) {
    await generateOGImage(page);
  }

  console.log("Done!");
}

main();

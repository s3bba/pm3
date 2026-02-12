import { docs } from "fumadocs-mdx:collections/server";
import type { InferPageType } from "fumadocs-core/source";
import { loader } from "fumadocs-core/source";
import { lucideIconsPlugin } from "fumadocs-core/source/lucide-icons";

export const source = loader({
  source: docs.toFumadocsSource(),
  baseUrl: "/docs",
  plugins: [lucideIconsPlugin()],
});

export function getPageImage(page: InferPageType<typeof source>) {
  const slug = page.slugs.join("/");
  return `/og/docs/${slug}.png`;
}

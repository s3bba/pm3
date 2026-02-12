import {
  DocsBody,
  DocsDescription,
  DocsPage,
  DocsTitle,
} from "fumadocs-ui/layouts/docs/page";
import defaultMdxComponents from "fumadocs-ui/mdx";
import type { PageProps } from "waku/router";
import { getPageImage, source } from "@/lib/source";

export default function DocPage({ slugs }: PageProps<"/docs/[...slugs]">) {
  const page = source.getPage(slugs);

  if (!page) {
    return (
      <div className="text-center py-12">
        <h1 className="text-3xl font-bold mb-4 text-gray-900 dark:text-gray-100">
          Page Not Found
        </h1>
        <p className="text-gray-600 dark:text-gray-400">
          The page you are looking for does not exist.
        </p>
      </div>
    );
  }

  const ogImage = getPageImage(page);
  const MDX = page.data.body;
  return (
    <DocsPage toc={page.data.toc} tableOfContent={{ style: "clerk" }}>
      <title>{`${page.data.title} - pm3`}</title>
      <meta property="og:title" content={page.data.title} />
      {page.data.description && (
        <meta property="og:description" content={page.data.description} />
      )}
      <meta property="og:image" content={ogImage} />
      <meta name="twitter:card" content="summary_large_image" />
      <meta name="twitter:image" content={ogImage} />
      <DocsTitle>{page.data.title}</DocsTitle>
      <DocsDescription>{page.data.description}</DocsDescription>
      <DocsBody>
        <MDX
          components={{
            ...defaultMdxComponents,
          }}
        />
      </DocsBody>
    </DocsPage>
  );
}

export async function getConfig() {
  const pages = source
    .generateParams()
    .map((item) => (item.lang ? [item.lang, ...item.slug] : item.slug));

  return {
    render: "static" as const,
    staticPaths: pages,
  } as const;
}

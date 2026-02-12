import { ConfigBuilder } from "@/components/config-builder";

export default function ConfigBuilderPage() {
  return <ConfigBuilder />;
}

export const getConfig = async () => {
  return { render: "static" };
};

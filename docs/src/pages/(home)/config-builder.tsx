import { ConfigBuilder } from "@/components/config-builder";

export default function ConfigBuilderPage() {
  return (
    <>
      <title>Config Builder - pm3</title>
      <ConfigBuilder />
    </>
  );
}

export const getConfig = async () => {
  return { render: "static" };
};

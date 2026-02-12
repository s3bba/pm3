"use client";

import { useEffect, useRef } from "react";

export function AsciinemaPlayer() {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!containerRef.current) return;
    const script = document.createElement("script");
    script.src = "https://asciinema.org/a/4Kat5eEd2jJPxTaz.js";
    script.id = "asciicast-4Kat5eEd2jJPxTaz";
    script.async = true;
    containerRef.current.appendChild(script);
  }, []);

  return <div ref={containerRef} />;
}

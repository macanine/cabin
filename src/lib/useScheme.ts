import { useEffect, useState } from "react";

/** The colour scheme in effect: an explicit `data-theme` wins over the system. */
function current(): "light" | "dark" {
  const root = document.documentElement;
  const forced = root.dataset.theme ?? root.dataset.mantineColorScheme;
  if (forced === "dark" || forced === "light") return forced;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/** Follows the app's light/dark scheme, for libraries that need it as a prop. */
export function useScheme(): "light" | "dark" {
  const [scheme, setScheme] = useState(current);
  useEffect(() => {
    const update = () => setScheme(current());
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    media?.addEventListener("change", update);
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme", "data-mantine-color-scheme"] });
    return () => { media?.removeEventListener("change", update); observer.disconnect(); };
  }, []);
  return scheme;
}

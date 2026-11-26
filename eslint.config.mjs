import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "build/**",
      "node_modules/**",
      "src-tauri/**",
      "tests/ui/**",
      "src/services/ipc/generated/**",
      // Rust-side trees and vendored JavaScript (spice-html5, shoes).
      "vm/**",
      "core/**",
      "usage/**",
      "ip-detection/**",
      "diagnostics/**",
      "third_party/**",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    plugins: { "react-hooks": reactHooks },
    rules: {
      // The classic hook rules only. The React Compiler rules in the
      // plugin's `recommended` preset (set-state-in-effect, purity) describe a
      // refactor this codebase has not taken on yet.
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
      // TypeScript already reports unused locals and parameters, with better
      // messages and an underscore convention.
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
  {
    files: ["tests/**/*.mjs", "scripts/**/*.mjs"],
    languageOptions: { globals: { ...globals.node } },
  },
);

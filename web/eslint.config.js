import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: ["dist", "dist.next", "dist.previous", "node_modules"],
  },
  {
    files: ["**/*.{js,mjs,ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      globals: {
        ...globals.browser,
        ...globals.node,
      },
    },
    plugins: {
      "react-hooks": reactHooks,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // These React Compiler-oriented rules reject established, intentional state synchronization
      // patterns in this UI. Keep the correctness-focused Rules of Hooks and exhaustive deps active.
      "react-hooks/set-state-in-effect": "off",
    },
  },
);

import js from '@eslint/js';
import prettier from 'eslint-config-prettier';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

export default ts.config(
  { ignores: ['dist/**', 'src/lib/api/schema.d.ts', 'node_modules/**'] },
  js.configs.recommended,
  ...ts.configs.recommended,
  ...svelte.configs['flat/recommended'],
  { languageOptions: { globals: { ...globals.browser, ...globals.node } } },
  {
    files: ['**/*.svelte'],
    languageOptions: { parserOptions: { parser: ts.parser } },
  },
  {
    rules: {
      '@typescript-eslint/no-unused-vars': [
        'error',
        { varsIgnorePattern: '^_' },
      ],
    },
  },
  prettier,
);

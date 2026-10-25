/**
 * A theme file as a stylesheet the browser can load on its own.
 *
 * The theme compiled in is read by Tailwind, whose `@theme` turns tokens into
 * custom properties on `:root`. A theme chosen at startup is not compiled --
 * one image serves every deployment -- so the build also writes each theme
 * out with `@theme` replaced by `:root`. The result is unlayered, which puts
 * it above Tailwind's own `@layer theme` and so wins over the built-in values
 * without an `!important` anywhere. That a theme sets tokens and paints
 * nothing (themes.test.ts) is what makes the substitution the whole job.
 */
export function plainThemeCss(css: string): string {
  return css.replace(/@theme(\s+static)?\s*\{/g, ':root {')
}

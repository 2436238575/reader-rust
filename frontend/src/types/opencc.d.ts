// opencc-js 未附带 TypeScript 声明；这里只声明用到的子路径与 API。
declare module 'opencc-js/cn2t' {
  export function Converter(options: { from: string; to: string }): (text: string) => string;
}

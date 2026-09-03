import { PrismLight } from "react-syntax-highlighter";
import bash from "react-syntax-highlighter/dist/esm/languages/prism/bash";
import css from "react-syntax-highlighter/dist/esm/languages/prism/css";
import go from "react-syntax-highlighter/dist/esm/languages/prism/go";
import java from "react-syntax-highlighter/dist/esm/languages/prism/java";
import javascript from "react-syntax-highlighter/dist/esm/languages/prism/javascript";
import json from "react-syntax-highlighter/dist/esm/languages/prism/json";
import jsx from "react-syntax-highlighter/dist/esm/languages/prism/jsx";
import kotlin from "react-syntax-highlighter/dist/esm/languages/prism/kotlin";
import markdown from "react-syntax-highlighter/dist/esm/languages/prism/markdown";
import markup from "react-syntax-highlighter/dist/esm/languages/prism/markup";
import powershell from "react-syntax-highlighter/dist/esm/languages/prism/powershell";
import python from "react-syntax-highlighter/dist/esm/languages/prism/python";
import rust from "react-syntax-highlighter/dist/esm/languages/prism/rust";
import sql from "react-syntax-highlighter/dist/esm/languages/prism/sql";
import toml from "react-syntax-highlighter/dist/esm/languages/prism/toml";
import tsx from "react-syntax-highlighter/dist/esm/languages/prism/tsx";
import typescript from "react-syntax-highlighter/dist/esm/languages/prism/typescript";
import yaml from "react-syntax-highlighter/dist/esm/languages/prism/yaml";

// 完整 Prism 会把数百种语言一起打进对话工作区。这里只注册产品里常见的语言,
// 未命中的代码块仍以纯文本正常展示,避免为低频格式支付首屏解析成本。
const languages = {
  bash,
  css,
  go,
  java,
  javascript,
  json,
  jsx,
  kotlin,
  markdown,
  markup,
  powershell,
  python,
  rust,
  sql,
  toml,
  tsx,
  typescript,
  yaml,
};

for (const [name, grammar] of Object.entries(languages)) {
  PrismLight.registerLanguage(name, grammar);
}
PrismLight.registerLanguage("js", javascript);
PrismLight.registerLanguage("ts", typescript);
PrismLight.registerLanguage("py", python);
PrismLight.registerLanguage("sh", bash);
PrismLight.registerLanguage("shell", bash);
PrismLight.registerLanguage("html", markup);
PrismLight.registerLanguage("xml", markup);
PrismLight.registerLanguage("yml", yaml);
PrismLight.registerLanguage("ps1", powershell);

export { PrismLight as CodeHighlighter };

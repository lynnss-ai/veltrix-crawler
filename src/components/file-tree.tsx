// 文件树:把平铺的相对路径列表(如 "src/components/App.tsx")组织成可折叠的树形结构。
// 纯展示组件:折叠状态内部维护(默认全展开),选中态由父组件控制;分栏面板与编程页共用。
import { useMemo, useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  FileText,
  Folder,
  FolderOpen,
} from "lucide-react";

interface FileNode {
  /** 本段名称(目录名或文件名) */
  name: string;
  /** 完整相对路径(目录也带,作为折叠状态的 key) */
  path: string;
  /** 子节点;非空即目录 */
  children: FileNode[];
}

/** 相对路径列表 → 树。同级排序:目录在前,名称按中文习惯(拼音)排。 */
function buildTree(files: string[]): FileNode[] {
  const roots: FileNode[] = [];
  const dirMap = new Map<string, FileNode>(); // 目录路径 -> 节点,复用已建目录
  for (const file of files) {
    const parts = file.split("/").filter(Boolean);
    let siblings = roots;
    let prefix = "";
    for (let i = 0; i < parts.length; i++) {
      const name = parts[i];
      prefix = prefix ? `${prefix}/${name}` : name;
      if (i === parts.length - 1) {
        siblings.push({ name, path: prefix, children: [] });
      } else {
        let dir = dirMap.get(prefix);
        if (!dir) {
          dir = { name, path: prefix, children: [] };
          dirMap.set(prefix, dir);
          siblings.push(dir);
        }
        siblings = dir.children;
      }
    }
  }
  const sortNodes = (nodes: FileNode[]) => {
    nodes.sort((a, b) => {
      const dirA = a.children.length > 0 ? 0 : 1;
      const dirB = b.children.length > 0 ? 0 : 1;
      if (dirA !== dirB) return dirA - dirB;
      return a.name.localeCompare(b.name, "zh");
    });
    for (const n of nodes) sortNodes(n.children);
  };
  sortNodes(roots);
  return roots;
}

export function FileTree({
  files,
  selected,
  onSelect,
}: {
  /** 工作区相对路径列表("/" 分隔) */
  files: string[];
  /** 当前选中的文件完整路径 */
  selected: string | null;
  onSelect: (path: string) => void;
}) {
  const tree = useMemo(() => buildTree(files), [files]);
  // 折叠的目录路径集合;默认全展开
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  function toggle(path: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function renderNodes(nodes: FileNode[], depth: number) {
    return nodes.map((node) => {
      const isDir = node.children.length > 0;
      const indent = depth * 12 + 6;
      if (isDir) {
        const isCollapsed = collapsed.has(node.path);
        return (
          <div key={node.path}>
            <button
              type="button"
              onClick={() => toggle(node.path)}
              title={node.path}
              style={{ paddingLeft: indent }}
              className="flex w-full items-center gap-1 rounded py-1 pr-2 text-left text-xs text-foreground transition-colors hover:bg-accent/50"
            >
              {isCollapsed ? (
                <ChevronRight className="size-3 shrink-0 text-muted-foreground" />
              ) : (
                <ChevronDown className="size-3 shrink-0 text-muted-foreground" />
              )}
              {isCollapsed ? (
                <Folder className="size-3.5 shrink-0 text-muted-foreground" />
              ) : (
                <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
              )}
              <span className="truncate">{node.name}</span>
            </button>
            {!isCollapsed && renderNodes(node.children, depth + 1)}
          </div>
        );
      }
      return (
        <button
          key={node.path}
          type="button"
          onClick={() => onSelect(node.path)}
          title={node.path}
          style={{ paddingLeft: indent + 12 }} // 与目录行的图标位对齐
          className={`flex w-full items-center gap-1.5 rounded py-1 pr-2 text-left text-xs transition-colors ${
            selected === node.path
              ? "bg-primary/10 font-medium text-primary"
              : "text-foreground hover:bg-accent/50"
          }`}
        >
          <FileText className="size-3.5 shrink-0" />
          <span className="truncate">{node.name}</span>
        </button>
      );
    });
  }

  return <>{renderNodes(tree, 0)}</>;
}

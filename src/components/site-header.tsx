// 顶部栏:两级面包屑(所属分组 › 当前页)。
// 主题切换 / 远程连接已移到窗口标题栏(TitleBar);侧栏开关也在 TitleBar。
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";

export function SiteHeader({ group, page }: { group: string; page: string }) {
  return (
    <header className="flex h-14 shrink-0 items-center gap-2 border-b px-4 lg:px-6">
      <Breadcrumb>
        <BreadcrumbList>
          {group && (
            <>
              <BreadcrumbItem className="hidden text-muted-foreground md:block">
                {group}
              </BreadcrumbItem>
              <BreadcrumbSeparator className="hidden md:block" />
            </>
          )}
          <BreadcrumbItem>
            <BreadcrumbPage>{page}</BreadcrumbPage>
          </BreadcrumbItem>
        </BreadcrumbList>
      </Breadcrumb>
    </header>
  );
}

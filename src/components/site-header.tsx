// 顶部栏:侧栏触发器 + 两级面包屑(所属分组 › 当前页) + 右侧主题切换。
import { SidebarTrigger } from "@/components/ui/sidebar";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";
import { ModeToggle } from "@/components/mode-toggle";

export function SiteHeader({ group, page }: { group: string; page: string }) {
  return (
    <header className="flex h-14 shrink-0 items-center gap-2 border-b px-4 lg:px-6">
      <SidebarTrigger className="-ml-1" />
      <Breadcrumb className="ml-1">
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
      <div className="ml-auto">
        <ModeToggle />
      </div>
    </header>
  );
}

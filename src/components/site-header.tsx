// 顶部栏:两级面包屑(所属分组 › 当前页) + 右侧主题切换 / 远程控制。
// 侧栏收起/展开开关已上移到窗口标题栏(TitleBar)。
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";
import { ModeToggle } from "@/components/mode-toggle";
import {
  RemoteConnectButton,
  type RemoteStatus,
} from "@/components/RemoteConnect";

export function SiteHeader({
  group,
  page,
  remoteStatus,
}: {
  group: string;
  page: string;
  remoteStatus: RemoteStatus;
}) {
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
      <div className="ml-auto flex items-center gap-1.5">
        <RemoteConnectButton status={remoteStatus} />
        <ModeToggle />
      </div>
    </header>
  );
}

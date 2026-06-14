; NSIS 安装/卸载自定义钩子(Tauri v2)。
; 解决「卸载后 %APPDATA% 下数据(配置 / 数据库 / 媒体 / WebView 数据)残留」的问题。
;
; 卸载后(POSTUNINSTALL)弹框询问是否一并删除用户数据;选「是」则递归删除应用数据目录。
;
; ⚠️ 关键:程序更新(应用内自动更新 / 覆盖安装)会**静默**调用旧卸载器。
; 静默场景一律跳过删除,保证更新绝不清空用户数据;只有用户交互式卸载才会询问。

!macro NSIS_HOOK_POSTUNINSTALL
  ; 静默运行 = 更新场景 → 直接跳过,绝不删数据
  IfSilent SkipUserData
  MessageBox MB_YESNO|MB_ICONQUESTION "是否同时删除本应用的全部数据?$\r$\n$\r$\n包含:采集内容、数据库、已下载的媒体文件、登录态与配置。$\r$\n删除后不可恢复;若打算重装并保留数据,请选「否」。" /SD IDNO IDNO SkipUserData
    ; 主数据目录:%APPDATA%\<identifier>(config / veltrix.db / media / webview-data / cloud.json 都在此)
    RMDir /r "$APPDATA\com.lynns.veltrix-crawler"
    ; WebView2 / 缓存类数据可能落在 %LOCALAPPDATA%\<identifier>
    RMDir /r "$LOCALAPPDATA\com.lynns.veltrix-crawler"
  SkipUserData:
!macroend

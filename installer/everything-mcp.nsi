; =====================================================================
; everything-mcp.nsi — NSIS 安装脚本
; ---------------------------------------------------------------------
; 用法：
;   1. 先用 cargo 构建出 everything_mcp.dll（见 README.md）。
;   2. 把 DLL 复制为 installer\bin\everything_mcp64.dll。
;   3. 用 makensis 编译本脚本生成 everything-mcp-<ver>-setup.exe。
;
; 安装行为：
;   - 默认安装到 <Everything 安装目录>\Plugins\（与 etp_server64.dll、
;     http_server64.dll 等官方插件同一目录）。
;   - DLL 命名为 everything_mcp64.dll —— Everything 1.5 按
;     Plugins\<name>64.dll 约定加载 64 位插件（已实测验证）。
;   - 卸载时删除该 DLL 及随附文件。
; =====================================================================

!define APP_NAME "Everything MCP"
!define APP_VERSION "1.0.0"
!define APP_PUBLISHER "JOJO"
!define DLL_NAME "everything_mcp64.dll"
!define PLUGIN_DIR_NAME "Plugins"

; ------ 现代 UI ------
!include "MUI2.nsh"

Name "${APP_NAME} ${APP_VERSION}"
OutFile "everything-mcp-${APP_VERSION}-setup.exe"
InstallDir "$PROGRAMFILES64\Everything\plugins\${PLUGIN_DIR_NAME}"
InstallDirRegKey HKLM "Software\${APP_PUBLISHER}\${APP_NAME}" "InstallDir"
RequestExecutionLevel admin
ShowInstDetails show
ShowUnInstDetails show
Unicode True

; ------ 版本信息（嵌入到 exe 资源）------
VIProductVersion "1.0.0.0"
VIAddVersionKey "ProductName" "${APP_NAME}"
VIAddVersionKey "FileVersion" "${APP_VERSION}"
VIAddVersionKey "ProductVersion" "${APP_VERSION}"
VIAddVersionKey "CompanyName" "${APP_PUBLISHER}"
VIAddVersionKey "LegalCopyright" "MIT"

; ------ MUI 设置 ------
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "..\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

; ------ 安装区段 ------
Section "Install" SecInstall
  SectionIn RO

  ; 安装目录
  SetOutPath "$INSTDIR"

  ; 复制 DLL
  File "bin\${DLL_NAME}"

  ; 复制配置示例与说明（中英文双 README）
  File "client-config-example.json"
  File "..\README.md"
  File "..\README.en.md"

  ; 写注册表项（用于卸载与升级查找）
  WriteRegStr HKLM "Software\${APP_PUBLISHER}\${APP_NAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
      "DisplayName" "${APP_NAME}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
      "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
      "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}" \
      "Publisher" "${APP_PUBLISHER}"

  ; 写卸载程序
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

; ------ 卸载区段 ------
Section "Uninstall"
  ; 只删除我们安装的文件；$INSTDIR（Everything\Plugins）不属于本插件，
  ; 里面还有其他插件，绝不能删。
  Delete "$INSTDIR\${DLL_NAME}"
  Delete "$INSTDIR\client-config-example.json"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\README.en.md"
  Delete "$INSTDIR\uninstall.exe"

  DeleteRegKey HKLM "Software\${APP_PUBLISHER}\${APP_NAME}"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_NAME}"
SectionEnd

; ------ 安装完成后给出客户端配置提示 ------
Function .onInstSuccess
  MessageBox MB_YESNO|MB_ICONQUESTION \
      "${APP_NAME} 已安装到 $INSTDIR$\r$\n$\r$\n要查看 MCP 客户端（如 Claude Desktop）的接入示例吗？" \
      IDNO skip
      ExecShell "open" "notepad.exe" "$INSTDIR\client-config-example.json"
  skip:
FunctionEnd

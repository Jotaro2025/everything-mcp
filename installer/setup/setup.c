// =====================================================================
// setup.c -- everything-mcp setup launcher (one source for x86 and x64)
// ---------------------------------------------------------------------
// This follows the voidtools official plugin (http_server / etp_server)
// setup exactly: this program copies no files by itself. It only
//   1. locates Everything.exe (registry ExePath -> running instance ->
//      manual browse), then
//   2. relaunches Everything with -setup-plugin arguments, letting
//      Everything itself pull the bz2-compressed plugin DLL out of this
//      exe's resources, decompress it, install it into Plugins\ and
//      register it in the plugin list (so it can be uninstalled later
//      from Everything Options -> Plugins, exactly like official ones).
//
// The plugin DLL is embedded as IDR_DLL_BZ2 RCDATA by setup.rc. The
// architecture is chosen at compile time via _WIN64: the x64 build embeds
// everything_mcp64.dll, the x86 build embeds everything_mcp32.dll.
//
// Differences from the official version: Win9x unicows support is dropped,
// and the folder picker uses SHBrowseForFolderW (the official one prefers
// IFileOpenDialog and falls back to it).
// =====================================================================

#define SETUP_NAME								"Everything MCP"	// name shown in Everything's plugin list
#define SETUP_DESCRIPTION						"Search files with Everything from an MCP client"
#define SETUP_AUTHOR							"JOJO"
#define SETUP_VERSION							PLUGINVERSION
#define SETUP_LINK								"https://github.com/Jotaro2025/everything-mcp"

#if defined(_WIN64)
#define SETUP_DLL_NAME							"everything_mcp64.dll"
#else
#define SETUP_DLL_NAME							"everything_mcp32.dll"
#endif

#define SETUP_EVERYTHING_PROGRAM_NAME			"Everything"
#define SETUP_EVERYTHING_TASKBAR_NOTIFICATION	"EVERYTHING_TASKBAR_NOTIFICATION"
#define SETUP_EVERYTHING_TASKBAR_NOTIFICATION_1_5_ALPHA	"EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)"

#define SETUP_MAX_STRING		MAX_PATH
#define SETUP_MAX_COMMAND_LINE	32768

// compiler options
#pragma warning(disable : 4311) // type cast void * to unsigned int
#pragma warning(disable : 4312) // type cast unsigned int to void *
#pragma warning(disable : 4244) // warning C4244: 'argument' : conversion from 'LONG_PTR' to 'LONG', possible loss of data
#pragma warning(disable : 4201) // nonstandard extension used : nameless struct/union
#pragma warning(disable : 4313) // 'printf' : '%x' in format string conflicts with argument 2 of type 'line_t *'
#pragma warning(disable : 4996) // deprecation

// REQUIRES IE 5.01
#define _WIN32_IE 0x0501

// WINNT required for some header definitions.
// minimum is really 0x0400
#define _WIN32_WINNT 0x0501

#define WIN32_LEAN_AND_MEAN

#define _INC_CTYPE

#include <windows.h>
#include <shellapi.h>
#include <Psapi.h>
#include <shlobj.h> // SHBrowseForFolderW
#include "resource.h"
#include "version.h"

void setup_main(void);
int setup_get_reg_string(HKEY root_hkey, const char* key, const char* value, wchar_t* buf);
void setup_atow(const char* as, wchar_t* ws);
int setup_get_everything_exe_filename(wchar_t* everything_exe_filename_wbuf);
void setup_fatal(DWORD error_code, const char* msg);
void setup_cat_astring(wchar_t* wbuf, DWORD size_in_wchars, const char* as);
void setup_cat_wstring(wchar_t* wbuf, DWORD size_in_wchars, const wchar_t* ws);
void setup_cat_number(wchar_t* wbuf, DWORD size_in_wchars, DWORD number);
static int setup_browse_for_folder(wchar_t* everything_exe_filename_wbuf);
static void setup_copy_string(wchar_t* dst, const wchar_t* src);
static int setup_get_running_exe_filename(const char* window_class, wchar_t* everything_exe_filename_wbuf);

void setup_atow(const char* as, wchar_t* ws)
{
	DWORD run;
	const char* p;
	wchar_t* d;

	run = SETUP_MAX_STRING - 1;
	p = as;
	d = ws;

	while (run)
	{
		if (!*p)
		{
			break;
		}

		*d++ = *p;

		p++;
		run--;
	}

	*d = 0;
}

int setup_get_reg_string(HKEY root_hkey, const char* key, const char* value, wchar_t* buf)
{
	int ret;
	wchar_t key_wbuf[SETUP_MAX_STRING];
	HKEY hkey;

	ret = 0;
	setup_atow(key, key_wbuf);

	// no need to try KEY_WOW64_64KEY
	// because we will call loadlibrary on this exe which will fail because wrong bitness.
	if (RegOpenKeyExW(root_hkey, key_wbuf, 0, KEY_READ, &hkey) == ERROR_SUCCESS)
	{
		wchar_t value_wbuf[SETUP_MAX_STRING];
		DWORD buf_size;

		setup_atow(value, value_wbuf);
		buf_size = SETUP_MAX_STRING * sizeof(wchar_t);

		if (RegQueryValueExW(hkey, value_wbuf, 0, NULL, (BYTE*)buf, &buf_size) == ERROR_SUCCESS)
		{
			DWORD buf_wlen;

			buf_wlen = buf_size / sizeof(wchar_t);

			// NULL terminated?
			if (buf_wlen)
			{
				if (buf[buf_wlen - 1] == 0)
				{
					ret = 1;
				}
			}
		}

		RegCloseKey(hkey);
	}

	return ret;
}

static void setup_copy_string(wchar_t* dst, const wchar_t* src)
{
	wchar_t* d;
	const wchar_t* p;
	SIZE_T run;

	d = dst;
	p = src;
	run = SETUP_MAX_STRING - 1;

	while (*p)
	{
		if (!run)
		{
			break;
		}

		*d++ = *p;
		run--;

		p++;
	}

	*d = 0;
}

static int setup_browse_for_folder(wchar_t* everything_exe_filename_wbuf)
{
	BROWSEINFOW bi;
	ITEMIDLIST* iil;
	int ret;

	ret = 0;

	// BIF_USENEWUI wont bring the initial selection into view.
	// BIF_NOTRANSLATETARGETS we dont want symbolic links targets, we want the actual path.
	// BIF_NEWDIALOGSTYLE allows computer to be selected, and keeps the OK button focused which keeps the dialog focused. Otherwise keyboard focus is lost
	ZeroMemory(&bi, sizeof(BROWSEINFOW));
	bi.lpszTitle = L"Select Everything.exe Location";
	bi.pszDisplayName = L"";
	bi.ulFlags = BIF_USENEWUI | BIF_NOTRANSLATETARGETS | BIF_NEWDIALOGSTYLE;
	bi.hwndOwner = NULL;

	iil = (ITEMIDLIST*)SHBrowseForFolderW(&bi);

	if (iil)
	{
		if (SHGetPathFromIDListW(iil, everything_exe_filename_wbuf))
		{
			ret = 1;
		}

		ILFree(iil);
	}
	else
	{
		// cancelled.
		ExitProcess(1);
	}

	return ret;
}

static int setup_get_running_exe_filename(const char* window_class, wchar_t* everything_exe_filename_wbuf)
{
	int ret;
	HWND everything_hwnd;
	wchar_t class_name_wbuf[SETUP_MAX_STRING];

	ret = 0;

	setup_atow(window_class, class_name_wbuf);

	everything_hwnd = FindWindowW(class_name_wbuf, NULL);
	if (everything_hwnd)
	{
		DWORD process_id;

		if (GetWindowThreadProcessId(everything_hwnd, &process_id))
		{
			HANDLE process_handle;

			process_handle = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, FALSE, process_id);
			if (process_handle)
			{
				if (GetModuleFileNameExW(process_handle, NULL, everything_exe_filename_wbuf, SETUP_MAX_STRING))
				{
					ret = 1;
				}

				CloseHandle(process_handle);
			}
		}
	}

	return ret;
}

int setup_get_everything_exe_filename(wchar_t* everything_exe_filename_wbuf)
{
	int ret;

	ret = 0;

	if (!ret)
	{
		// get everything install folder
		if (setup_get_reg_string(HKEY_LOCAL_MACHINE, "SOFTWARE\\voidtools\\"SETUP_EVERYTHING_PROGRAM_NAME, "ExePath", everything_exe_filename_wbuf))
		{
			ret = 1;
		}
	}

	// portable version running?
	if (!ret)
	{
		if (setup_get_running_exe_filename(SETUP_EVERYTHING_TASKBAR_NOTIFICATION, everything_exe_filename_wbuf))
		{
			ret = 1;
		}
	}

	// 1.5a version running?
	if (!ret)
	{
		if (setup_get_running_exe_filename(SETUP_EVERYTHING_TASKBAR_NOTIFICATION_1_5_ALPHA, everything_exe_filename_wbuf))
		{
			ret = 1;
		}
	}

	if (!ret)
	{
		if (setup_browse_for_folder(everything_exe_filename_wbuf))
		{
			ret = 1;
		}
	}

	return ret;
}

void setup_fatal(DWORD error_code, const char* msg)
{
	wchar_t title_wbuf[SETUP_MAX_STRING];
	wchar_t msg_wbuf[SETUP_MAX_STRING];

	title_wbuf[0] = 0;
	msg_wbuf[0] = 0;

	setup_cat_astring(title_wbuf, SETUP_MAX_STRING, "Setup Everything Plug-in");
	setup_cat_astring(msg_wbuf, SETUP_MAX_STRING, "Error ");
	setup_cat_number(msg_wbuf, SETUP_MAX_STRING, error_code);
	setup_cat_astring(msg_wbuf, SETUP_MAX_STRING, ": ");
	setup_cat_astring(msg_wbuf, SETUP_MAX_STRING, msg);

	MessageBoxW(0, msg_wbuf, title_wbuf, MB_ICONERROR | MB_OK);

	ExitProcess(1);
}

void* setup_alloc(DWORD size)
{
	void* p;

	p = HeapAlloc(GetProcessHeap(), 0, size);
	if (!p)
	{
		setup_fatal(ERROR_OUTOFMEMORY, "Out of memory");
	}

	return p;
}

void setup_free(void* ptr)
{
	HeapFree(GetProcessHeap(), 0, ptr);
}

void setup_cat_astring(wchar_t* wbuf, DWORD size_in_wchars, const char* as)
{
	wchar_t* d;
	const char* p;
	DWORD run;

	run = size_in_wchars - 1;
	d = wbuf;

	while (run)
	{
		if (!*d)
		{
			break;
		}

		d++;
		run--;
	}

	p = as;
	while (run)
	{
		if (!*p)
		{
			break;
		}

		*d++ = *p;
		run--;

		p++;
	}

	*d = 0;
}

void setup_cat_wstring(wchar_t* wbuf, DWORD size_in_wchars, const wchar_t* ws)
{
	wchar_t* d;
	const wchar_t* p;
	DWORD run;

	run = size_in_wchars - 1;
	d = wbuf;

	while (run)
	{
		if (!*d)
		{
			break;
		}

		d++;
		run--;
	}

	p = ws;
	while (run)
	{
		if (!*p)
		{
			break;
		}

		*d++ = *p;
		run--;

		p++;
	}

	*d = 0;
}

void setup_cat_number(wchar_t* wbuf, DWORD size_in_wchars, DWORD number)
{
	char number_buf[SETUP_MAX_STRING];
	char* d;

	d = number_buf + SETUP_MAX_STRING;
	d--;
	*d = 0;

	if (number)
	{
		while (number)
		{
			int i;

			i = number % 10;

			d--;
			*d = '0' + i;

			number /= 10;
		}
	}
	else
	{
		d--;
		*d = '0';
	}

	setup_cat_astring(wbuf, size_in_wchars, d);
}

void setup_main(void)
{
	wchar_t everything_exe_filename_wbuf[SETUP_MAX_STRING];

	// required for SHBrowseForFolder
	OleInitialize(0);

	// get everything install folder
	if (setup_get_everything_exe_filename(everything_exe_filename_wbuf))
	{
		wchar_t setup_exe_filename_wbuf[SETUP_MAX_STRING];

		if (GetModuleFileNameW(NULL, setup_exe_filename_wbuf, SETUP_MAX_STRING))
		{
			wchar_t* command_line_wbuf;
			STARTUPINFOW startup_info;
			PROCESS_INFORMATION process_information;

			command_line_wbuf = setup_alloc(SETUP_MAX_COMMAND_LINE * sizeof(wchar_t));

			*command_line_wbuf = 0;

			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, "\"");
			setup_cat_wstring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, everything_exe_filename_wbuf);
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, "\" ");

			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin \""SETUP_DLL_NAME"\"");

			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-exe \"");
			setup_cat_wstring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, setup_exe_filename_wbuf);
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, "\"");

			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-resource-id \"");
			setup_cat_number(command_line_wbuf, SETUP_MAX_COMMAND_LINE, IDR_DLL_BZ2);
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, "\"");

			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-name \""SETUP_NAME"\"");
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-description \""SETUP_DESCRIPTION"\"");
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-author \""SETUP_AUTHOR"\"");
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-version \""SETUP_VERSION"\"");
			setup_cat_astring(command_line_wbuf, SETUP_MAX_COMMAND_LINE, " -setup-plugin-link \""SETUP_LINK"\"");

			ZeroMemory(&startup_info, sizeof(STARTUPINFOW));
			startup_info.cb = sizeof(STARTUPINFOW);

			if (CreateProcessW(everything_exe_filename_wbuf, command_line_wbuf, NULL, NULL, FALSE, 0, NULL, NULL, &startup_info, &process_information))
			{
				// Everything handles any errors from here.
				CloseHandle(process_information.hThread);
				CloseHandle(process_information.hProcess);
			}
			else
			{
				setup_fatal(GetLastError(), "Unable to start Everything.");
			}

			setup_free(command_line_wbuf);
		}
		else
		{
			setup_fatal(GetLastError(), "Unable to get setup filename.");
		}
	}
	else
	{
		setup_fatal(ERROR_FILE_NOT_FOUND, "Unable to find Everything.\n\nPlease make sure Everything is installed or running.");
	}
}

int WINAPI WinMain(HINSTANCE hInstance, HINSTANCE hPrevInstance, LPSTR lpCmdLine, int nCmdShow)
{
	setup_main();

	ExitProcess(0);
}

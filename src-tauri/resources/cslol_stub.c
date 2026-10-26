/*
 * Stand-in for cslol-dll.dll.
 *
 * mod-tools.exe links cslol-dll.dll at load time, so Windows refuses
 * to start it without a DLL of that name. Veil uses ltk_patcher_host
 * for runtime injection instead of mod-tools.exe's "runoverlay" command
 * (which calls into cslol-dll.dll), so these exports only need to
 * exist. None of them do anything; each reports failure in case
 * something calls it anyway.
 *
 * Same approach as Rose: https://github.com/Alban1911/Rose
 *
 * Compile on Windows with MSVC:
 *   cl /LD cslol_stub.c /o cslol-dll.dll
 *
 * Or copy a pre-built cslol-dll.dll from a Rose install.
 */

#include <stddef.h>

#define EXPORT __declspec(dllexport)

static const char stub_error[] = "cslol-dll stub: runoverlay is not supported by Veil";

EXPORT const char* cslol_init(void) { return stub_error; }
EXPORT const char* cslol_set_config(const wchar_t* prefix) { (void)prefix; return stub_error; }
EXPORT const char* cslol_set_flags(unsigned long long flags) { (void)flags; return stub_error; }
EXPORT const char* cslol_set_log_level(unsigned int level) { (void)level; return stub_error; }
EXPORT unsigned int cslol_find(void) { return 0; }
EXPORT const char* cslol_hook(unsigned int tid, unsigned int timeout, unsigned int step) {
    (void)tid; (void)timeout; (void)step;
    return stub_error;
}
EXPORT const char* cslol_log_pull(void) { return NULL; }

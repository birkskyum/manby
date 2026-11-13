/* The package's library on Windows where manby is not built in: R there
   wants a library from a package with native code, so it has one that
   registers nothing, and the package runs the manby program. */

#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>

void R_init_manby(DllInfo *dll)
{
	R_registerRoutines(dll, NULL, NULL, NULL, NULL);
	R_useDynamicSymbols(dll, FALSE);
}

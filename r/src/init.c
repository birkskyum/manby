/* The package's native code, built only for webR, where manby runs in R's
   own process: src/rust is manby's command line, called from here. */

#include <R.h>
#include <Rinternals.h>
#include <R_ext/Rdynload.h>

int manby_r_run(int argc, const char **argv);
int manby_r_stats(int argc, const char **argv, char **out);
void manby_r_free(char *s);

static const char **arguments(SEXP args)
{
	int n = LENGTH(args);
	const char **argv = (const char **) R_alloc(n, sizeof(char *));
	for (int i = 0; i < n; i++) {
		argv[i] = translateCharUTF8(STRING_ELT(args, i));
	}
	return argv;
}

/* manby's command line: the exit status */
SEXP manby_run_(SEXP args)
{
	return ScalarInteger(manby_r_run(LENGTH(args), arguments(args)));
}

/* manby stats: the exit status and the reports */
SEXP manby_stats_(SEXP args)
{
	char *out = NULL;
	int status = manby_r_stats(LENGTH(args), arguments(args), &out);
	SEXP result = PROTECT(allocVector(VECSXP, 2));
	SET_VECTOR_ELT(result, 0, ScalarInteger(status));
	SET_VECTOR_ELT(result, 1, out ? mkString(out) : allocVector(STRSXP, 0));
	manby_r_free(out);
	UNPROTECT(1);
	return result;
}

static const R_CallMethodDef calls[] = {
	{"manby_run_", (DL_FUNC) &manby_run_, 1},
	{"manby_stats_", (DL_FUNC) &manby_stats_, 1},
	{NULL, NULL, 0}
};

void R_init_manby(DllInfo *dll)
{
	R_registerRoutines(dll, NULL, calls, NULL, NULL);
	R_useDynamicSymbols(dll, FALSE);
}

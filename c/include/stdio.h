#ifndef IC_SQLITE_STDIO_H
#define IC_SQLITE_STDIO_H

#include <stdarg.h>
#include <stddef.h>

#define EOF (-1)
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

typedef struct FILE FILE;

extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

int snprintf(char *s, size_t n, const char *format, ...);
int vsnprintf(char *s, size_t n, const char *format, va_list arg);
int printf(const char *format, ...);
int fprintf(FILE *stream, const char *format, ...);
int fflush(FILE *stream);

#endif

#ifndef IC_SQLITE_STDLIB_H
#define IC_SQLITE_STDLIB_H

#include <stddef.h>

void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
void abort(void);

int abs(int n);
long labs(long n);
long long llabs(long long n);
int atoi(const char *nptr);
double atof(const char *nptr);
long strtol(const char *nptr, char **endptr, int base);
long long strtoll(const char *nptr, char **endptr, int base);
unsigned long long strtoull(const char *nptr, char **endptr, int base);
double strtod(const char *nptr, char **endptr);
void qsort(void *base, size_t nmemb, size_t size,
           int (*compar)(const void *, const void *));
char *getenv(const char *name);
int system(const char *command);

#endif

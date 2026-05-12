#ifndef IC_SQLITE_ASSERT_H
#define IC_SQLITE_ASSERT_H

#ifdef NDEBUG
#define assert(expr) ((void)0)
#else
void abort(void);
#define assert(expr) ((expr) ? (void)0 : abort())
#endif

#endif
